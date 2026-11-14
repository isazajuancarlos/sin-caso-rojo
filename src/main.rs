// SPDX-License-Identifier: LicenseRef-Propietario
// SPDX-FileCopyrightText: 2026 Juan Carlos Isaza

//! **¿Qué trozos de este cambio NO cubre el banco?**
//!
//! # El problema que resuelve, medido
//!
//! El 2026-08-13, una revisión de seguridad de `inventario` encontró que de las
//! seis guardas nuevas de un commit —seis `ok_or`/`checked_add` que rechazan un
//! importe imposible— **ninguna tenía caso rojo**. Lo demostró de la única forma
//! que vale: revirtiéndolas a la conducta permisiva y viendo la suite quedarse
//! en 103/103 verde, con su positivo de control (romper el RESULTADO en vez de
//! la guarda ponía 7 pruebas rojas, así que la suite sí ejecutaba esas líneas).
//!
//! Eso costó una pasada entera de un modelo grande razonando, editando y
//! corriendo `cargo test` a mano. **Y no necesita un modelo: es determinista.**
//! Un `ok_or` que nunca dispara es indistinguible de uno cableado a la condición
//! equivocada (directiva 33), y saber cuál de los dos es se decide corriendo
//! pruebas, no leyendo.
//!
//! # Qué hace exactamente
//!
//! Para cada HUNK que el diff añade en código de producción:
//!
//! 1. lo revierte —y solo ése, no el commit entero—,
//! 2. corre el comando de pruebas que se le pase,
//! 3. lo restaura,
//! 4. y clasifica:
//!
//! | veredicto | qué significa |
//! |---|---|
//! | **CUBIERTO** | al revertirlo, algo se puso rojo. El banco vigila ese trozo |
//! | **SIN CASO ROJO** | al revertirlo, todo siguió verde. Nadie mide ese trozo |
//! | **NO COMPILA** | revertirlo solo rompe la compilación: no se puede saber |
//!
//! El tercero es la razón de que el veredicto tenga TRES valores y no dos. Un
//! hunk que al revertirse no compila no está «cubierto»: está **sin medir**, y
//! llamarlo verde sería exactamente el fallo que esta herramienta existe para
//! cazar (directiva 30: un negativo solo vale si el camino llegó a ejecutarse).
//!
//! # Lo que NO es
//!
//! No es un revisor: no opina, no busca vulnerabilidades y no lee el código.
//! Produce el **residuo** sobre el que un revisor —humano o modelo— tiene que
//! decidir, y le ahorra justo la parte cara y mecánica. Un hunk «sin caso rojo»
//! no es un defecto por sí mismo: hay guardas legítimamente inalcanzables. Lo
//! que no puede es pasar inadvertido.
//!
//! # Seguridad de la operación
//!
//! Toca el árbol de trabajo de OTRO repositorio, así que:
//!
//! - **se niega si el árbol no está limpio** (`git status --porcelain`), porque
//!   un fallo a mitad no puede distinguirse de trabajo sin guardar;
//! - restaura el hunk con un guardián que corre en `Drop`, lo que cubre el
//!   camino de error y el pánico — **pero NO un Ctrl-C**: sin manejador de
//!   señal, `SIGINT` termina el proceso y `Drop` no llega a correr. Aquí ponía
//!   «restaura **siempre**, incluido el camino de error», y era una promesa en
//!   absoluto que la revisión del 2026-08-13 midió y desmintió. Por eso el
//!   programa imprime el comando de recuperación ANTES de empezar: si lo cortas
//!   a mitad, ahí está;
//! - **comprueba al terminar** que el árbol volvió a estar limpio, y si no, lo
//!   dice a gritos con el comando exacto para recuperarlo.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

/// Un trozo contiguo del diff, con el archivo al que pertenece.
#[derive(Debug, Clone)]
struct Hunk {
    archivo: String,
    /// Línea inicial en el archivo NUEVO, para poder nombrarlo.
    linea: usize,
    /// El texto del hunk tal como lo escribe `git diff`, con su cabecera.
    texto: String,
    /// Cuántas líneas AÑADE. Un hunk que solo borra también se revierte, pero
    /// esto sirve para ordenar el informe por lo que más pesa.
    anadidas: usize,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Veredicto {
    Cubierto,
    SinCasoRojo,
    NoCompila,
    NoSePudoRevertir,
    /// Revertir el hunk RESUCITARÍA un archivo que la rama medida borró, y eso
    /// no se hace: se rechaza sin tocar nada.
    ReviviriaUnBorrado,
    /// El hunk no añade ni quita una sola línea EJECUTABLE: es prosa. Que el
    /// banco no lo mida es lo correcto, no un hallazgo.
    SoloComentarios,
    /// El banco se puso rojo al revertir el hunk **y siguió rojo al
    /// restaurarlo**: el rojo no venía del hunk. No hay veredicto.
    BancoInestable,
    /// El hunk cae ENTERO dentro de una región `#[cfg(test)]`: es código de
    /// prueba viviendo en un archivo de producción. Que ninguna otra prueba lo
    /// vigile es cierto por construcción y no dice nada, igual que la prosa.
    /// Con `--incluir-pruebas` se mide como cualquier otro.
    CodigoDePrueba,
}

impl Veredicto {
    fn etiqueta(self) -> &'static str {
        match self {
            Veredicto::Cubierto => "COVERED",
            Veredicto::SinCasoRojo => "NO RED TEST",
            Veredicto::NoCompila => "DOES NOT COMPILE (unmeasured)",
            Veredicto::NoSePudoRevertir => "COULD NOT REVERT",
            Veredicto::BancoInestable => "FLAKY SUITE (unmeasured)",
            Veredicto::ReviviriaUnBorrado => "WOULD REVIVE A DELETION (not measured)",
            Veredicto::SoloComentarios => "COMMENTS ONLY (n/a)",
            Veredicto::CodigoDePrueba => "TEST (n/a)",
        }
    }
}

struct Opciones {
    repo: PathBuf,
    base: String,
    rama: String,
    prueba: String,
    incluir_pruebas: bool,
    solo_listar: bool,
    /// No intentar `cargo build` antes de las pruebas.
    ///
    /// Existe desde el 2026-08-13 porque **la skill lo documentaba y no
    /// existía**: decía «en un repo sin `Cargo.toml` omite el paso de
    /// compilación», y no había forma de omitirlo. En un repositorio de
    /// JavaScript —`boi`, que el barrido incluye por `*.js`— eso hacía que
    /// TODOS los hunks salieran `NO COMPILA` y el soporte de `.js` fuera
    /// inalcanzable: la promesa y la máquina no coincidían.
    sin_compilar: bool,
}

fn main() {
    let opciones = match leer_argumentos() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("{e}\n");
            eprintln!("{}", USO);
            std::process::exit(2);
        }
    };

    match ejecutar(&opciones) {
        Ok(codigo) => std::process::exit(codigo),
        Err(e) => {
            eprintln!("\n\x1b[1;31mCOULD NOT MEASURE\x1b[0m: {e}");
            std::process::exit(2);
        }
    }
}

const USO: &str = "\
sin-caso-rojo — which parts of a change the test suite does NOT cover

    sin-caso-rojo <repo> --base <B> --branch <R> [options]

  --base <B>        commit or branch to compare against (e.g. main)
  --branch <R>      commit or branch to measure (e.g. HEAD, or a work branch)
  --test <CMD>      the test command. Default: `cargo test --workspace`
  --include-tests   also reverts hunks of test files (default NO:
                    reverting a test and seeing that 'nothing breaks' says nothing)
  --no-build        does not try `cargo build` before the tests. For
                    repositories that are not cargo-based (e.g. JavaScript). The
                    DOES NOT COMPILE verdict is no longer available, and it says so.
  --list            only shows the hunks it would measure, without running anything

Exit codes:
  0  all hunks covered, and all could be measured
  1  there are hunks with NO RED TEST — nobody measures them
  2  nothing could be measured (dirty tree, wrong branch, suite already red…)
  3  INCONCLUSIVE: none discovered, but some were left unmeasured";

fn leer_argumentos() -> Result<Opciones, String> {
    let mut args = std::env::args().skip(1);
    let mut repo = None;
    let mut base = None;
    let mut rama = None;
    let mut prueba = String::from("cargo test --workspace");
    let mut incluir_pruebas = false;
    let mut solo_listar = false;
    let mut sin_compilar = false;

    while let Some(a) = args.next() {
        // Flags are BILINGUAL: the primary name is English, the Spanish alias is
        // still accepted so that our internal callers (skills that invoke
        // `--prueba`, `--rama`, …) keep working. `--help` shows only English.
        match a.as_str() {
            "--base" => base = Some(args.next().ok_or("--base needs a value")?),
            "--branch" | "--rama" => rama = Some(args.next().ok_or("--branch needs a value")?),
            "--test" | "--prueba" => prueba = args.next().ok_or("--test needs a value")?,
            "--include-tests" | "--incluir-pruebas" => incluir_pruebas = true,
            "--no-build" | "--sin-compilar" => sin_compilar = true,
            "--list" | "--lista" => solo_listar = true,
            "-h" | "--help" => {
                println!("{USO}");
                std::process::exit(0);
            }
            otro if otro.starts_with("--") => return Err(format!("unknown option: {otro}")),
            otro => repo = Some(PathBuf::from(otro)),
        }
    }

    // Se exige TODO explícito. Un `--base` con valor por omisión («main») es la
    // clase de relleno que hace que alguien mida el diff equivocado y no se
    // entere (directiva 20).
    Ok(Opciones {
        repo: repo.ok_or("missing repository")?,
        base: base.ok_or("missing --base")?,
        rama: rama.ok_or("missing --branch")?,
        prueba,
        incluir_pruebas,
        solo_listar,
        sin_compilar,
    })
}

/// Corre `git` dentro del repositorio. Nunca depende del `cwd`: el harness lo
/// devuelve a otro sitio entre llamadas, y esa fue la causa de que una revisión
/// entera midiera el repositorio equivocado (2026-07-31).
fn git(repo: &Path, args: &[&str]) -> Result<String, String> {
    let salida = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|e| format!("could not run git: {e}"))?;
    if !salida.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&salida.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&salida.stdout).into_owned())
}

fn ejecutar(o: &Opciones) -> Result<i32, String> {
    if !o.repo.join(".git").exists() {
        return Err(format!("{} is not a git repository", o.repo.display()));
    }

    // EL ÁRBOL TIENE QUE ESTAR LIMPIO ANTES DE EMPEZAR. Si no, un fallo a mitad
    // deja al usuario sin poder distinguir lo que rompió esta herramienta de lo
    // que él tenía sin guardar.
    //
    // `--untracked-files=no`, y esto lo corrigió la prueba: la primera versión
    // miraba también lo NO RASTREADO y se negaba a medir cualquier repositorio
    // con `target/` o un `Cargo.lock` sin commitear — que es el estado normal de
    // una cobaya recién creada, y de más de un repo real. Lo no rastreado no
    // estorba: esta herramienta solo revierte hunks de archivos rastreados y los
    // restaura con `git checkout --`, que no puede tocar lo que git no sigue.
    let sucio = git(&o.repo, &["status", "--porcelain", "--untracked-files=no"])?;
    if !sucio.trim().is_empty() {
        return Err(format!(
            "the working tree has unsaved changes and this tool is going to \
             modify it temporarily. Stash or discard first:\n{}",
            sucio.trim()
        ));
    }

    // EL ÁRBOL TIENE QUE ESTAR EN `--rama`, y esto lo encontró su propia
    // corrida de control: midiendo `95b1a00..44a3ffd` con el árbol en `main`,
    // CINCO de trece hunks salieron «NO SE PUDO REVERTIR» porque el contexto de
    // los parches no casaba con los archivos de delante.
    //
    // Se falla en vez de hacer `checkout` solo: cambiar de rama el repositorio
    // de otro por su cuenta es de las cosas que no se deshacen si algo sale a
    // mitad, y esta herramienta ya edita bastante. Que lo decida quien llama.
    //
    // El veredicto de tres valores es lo que evitó que esto pasara por bueno: si
    // «no se pudo revertir» se hubiera colapsado en «cubierto», la herramienta
    // habría contestado que cinco hunks estaban vigilados sin haberlos tocado
    // (directiva 30).
    // Las revisiones se VALIDAN antes de entrar en ningún `format!`. Un valor
    // como `--output=…` sería inyección de argumento a `git diff`. La entrada es
    // del invocador y por tanto confiable, así que esto es barandilla, no muro —
    // pero cuesta dos líneas y lo señaló la revisión del 2026-08-13.
    for r in [&o.base, &o.rama] {
        if r.starts_with('-') {
            return Err(format!(
                "«{r}» cannot be a revision: it starts with a dash and git would read it \
                 as an option"
            ));
        }
        git(&o.repo, &["rev-parse", "--verify", "--quiet", &format!("{r}^{{commit}}")])
            .map_err(|_| format!("«{r}» is not a commit of this repository"))?;
    }

    let cabeza = git(&o.repo, &["rev-parse", "HEAD"])?.trim().to_string();
    let objetivo = git(&o.repo, &["rev-parse", &o.rama])?.trim().to_string();
    if cabeza != objetivo {
        return Err(format!(
            "the working tree is at {} and {} ({}) needs measuring.\n\
             Patches apply against the files in front, so with a different \
             branch checked out half of them don't match and the verdict is worthless.\n  \
             git -C {} checkout {}",
            &cabeza[..12.min(cabeza.len())],
            o.rama,
            &objetivo[..12.min(objetivo.len())],
            o.repo.display(),
            o.rama
        ));
    }

    let (hunks, descartados) = recoger_hunks(o)?;
    if hunks.is_empty() {
        // NO devuelve 0. Un `if rc == 0` daría por verde una medición que no
        // ocurrió, que es el mismo fallo que el código 3 vino a cerrar para los
        // hunks sin medir — y seguía abierto por esta puerta (revisión del
        // 2026-08-13).
        println!(
            "INCONCLUSIVE — the diff has no measurable hunk.\n  \
             {descartados} file(s) in the diff were left out by extension: this \
             sweep only looks at `*.rs` and `*.js`.\n  \
             Nothing was measured, so there is no verdict."
        );
        return Ok(3);
    }

    println!("{}", cabecera(o, &hunks, descartados)?);

    // EL COMANDO DE RECUPERACIÓN, ANTES DE TOCAR NADA. Esta herramienta invita a
    // interrumpirla —dice cuántos minutos va a tardar— y un Ctrl-C deja el hunk
    // revertido, porque `Drop` no corre ante `SIGINT`. Si lo cortas, esto es lo
    // que hay que hacer, y está impreso arriba y no en un README.
    println!(
        "If you interrupt it midway, the hunk stays reverted. Recover it with:\n  \
         git -C {} checkout -- . && git -C {} clean -f\n",
        o.repo.display(),
        o.repo.display()
    );

    if o.solo_listar {
        for h in &hunks {
            println!("  {}:{}  (+{} lines)", h.archivo, h.linea, h.anadidas);
        }
        return Ok(0);
    }

    // POSITIVO DE CONTROL, y va PRIMERO. Si el banco no está verde de partida,
    // todo lo que sigue es ruido: cada hunk saldría «cubierto» por un fallo que
    // ya estaba ahí. Es la directiva 30 aplicada a la propia herramienta.
    print!("Control: is the suite green WITHOUT touching anything? ");
    use std::io::Write as _;
    std::io::stdout().flush().ok();
    let t0 = Instant::now();
    if !correr_pruebas(o)? {
        println!("NO");
        return Err(format!(
            "the suite is already RED before reverting anything, so no verdict \
             below would mean a thing. Fix `{}` first.",
            o.prueba
        ));
    }
    let duracion = t0.elapsed();
    println!("yes ({:.1}s per run)", duracion.as_secs_f64());

    // LA FOTO DEL ÁRBOL SE TOMA AQUÍ, después de la corrida de control y no
    // antes. Esa primera corrida crea `Cargo.lock`, `target/` y lo que el banco
    // deje, así que una foto anterior marcaría todo eso como «residuo» de la
    // medición. Lo que se quiere comparar es el estado tras UNA corrida normal
    // del banco contra el estado tras TODAS.
    let huella_inicial = git(&o.repo, &["status", "--porcelain"])?;
    println!(
        "Going to do {} runs: ~{:.0} min.\n",
        hunks.len(),
        duracion.as_secs_f64() * hunks.len() as f64 / 60.0
    );

    let mut veredictos: Vec<(Hunk, Veredicto)> = Vec::new();
    for (i, h) in hunks.iter().enumerate() {
        print!("[{}/{}] {}:{} … ", i + 1, hunks.len(), h.archivo, h.linea);
        std::io::stdout().flush().ok();
        let v = medir_hunk(o, h)?;
        println!("{}", v.etiqueta());
        veredictos.push((h.clone(), v));
    }

    // Y AL TERMINAR, se comprueba que el árbol volvió a estar como estaba. Una
    // herramienta que edita el código ajeno y no lo verifica al salir es una que
    // algún día deja el repositorio a medias sin decirlo.
    // LA COMPROBACIÓN FINAL MIRA TAMBIÉN LO NO RASTREADO, y se compara contra
    // la foto de ENTRADA en vez de exigir vacío.
    //
    // Antes usaba `--untracked-files=no`, o sea que era ciega exactamente al
    // residuo que la herramienta produce (un archivo resucitado nace sin
    // rastrear). Y no se puede exigir «vacío» a secas porque el repositorio
    // legítimamente tiene `target/`, `Cargo.lock` y demás: lo que hay que
    // comprobar es que NO APAREZCA NADA NUEVO.
    let sucio_final = git(&o.repo, &["status", "--porcelain"])?;
    let antes: std::collections::BTreeSet<&str> = huella_inicial.lines().collect();
    let nuevo: Vec<&str> = sucio_final
        .lines()
        .filter(|l| !antes.contains(l))
        .collect();
    if !nuevo.is_empty() {
        return Err(format!(
            "THE TREE WAS LEFT WITH RESIDUE and that should NOT happen. Check it and recover it:\n  \
             git -C {} status\nWhat appeared during the measurement:\n{}",
            o.repo.display(),
            nuevo.join("\n")
        ));
    }

    println!("\n{}", informe(&veredictos));
    // TRES desenlaces, no dos, por la misma razón que los canarios del taller:
    // «sano» y «no se pudo medir» tienen que verse distinto desde fuera, o un
    // guion que solo mire `if rc == 0` dará por bueno lo que nadie midió.
    let hay_descubiertos = veredictos.iter().any(|(_, v)| *v == Veredicto::SinCasoRojo);
    let hay_sin_medir = veredictos
        .iter()
        .any(|(_, v)| {
            matches!(
                v,
                Veredicto::NoCompila
                    | Veredicto::NoSePudoRevertir
                    | Veredicto::BancoInestable
                    | Veredicto::ReviviriaUnBorrado
            )
        });
    Ok(match (hay_descubiertos, hay_sin_medir) {
        (true, _) => 1,  // hay hunks que nadie mide
        (false, true) => 3,  // no concluyente
        (false, false) => 0, // todo cubierto de verdad
    })
}

fn cabecera(o: &Opciones, hunks: &[Hunk], descartados: usize) -> Result<String, String> {
    let sha_base = git(&o.repo, &["rev-parse", "--short", &o.base])?;
    let sha_rama = git(&o.repo, &["rev-parse", "--short", &o.rama])?;
    let mut s = String::new();
    let _ = writeln!(s, "{}", "=".repeat(72));
    let _ = writeln!(s, "sin-caso-rojo — what the suite does NOT measure in this change");
    let _ = writeln!(s, "{}", "=".repeat(72));
    let _ = writeln!(s, "repository : {}", o.repo.display());
    let _ = writeln!(s, "base       : {}  ({})", o.base, sha_base.trim());
    let _ = writeln!(s, "branch     : {}  ({})", o.rama, sha_rama.trim());
    let _ = writeln!(s, "test       : {}", o.prueba);
    let _ = writeln!(
        s,
        "hunks      : {} {}",
        hunks.len(),
        if o.incluir_pruebas {
            "(including those from test files)"
        } else {
            "(production code only)"
        }
    );
    // Cuántos archivos del diff NO se miraron por su extensión. Se dice SIEMPRE,
    // también cuando son cero: «no había» y «no los miré» tienen que verse
    // distinto (directiva 30).
    let _ = writeln!(
        s,
        "excluded   : {descartados} file(s) in the diff, by extension (only *.rs and *.js are looked at)"
    );
    if o.sin_compilar {
        let _ = writeln!(
            s,
            "NOTE       : `--no-build` — there is no compilation step, so the\n\
             \x20            DOES NOT COMPILE verdict is unavailable: a compilation\n\
             \x20            failure will show up as a red suite, i.e. as COVERED."
        );
    }
    let _ = writeln!(s, "{}", "=".repeat(72));
    Ok(s)
}

/// ¿Es este archivo código de prueba?
///
/// Se decide por la RUTA y no por el contenido, a propósito: un `#[cfg(test)]`
/// dentro de `src/` es código de prueba que vive en un archivo de producción, y
/// revertir un hunk de esa región es una medición legítima —lo que se comprueba
/// entonces es si el resto del banco depende de esa prueba—. Lo que no tiene
/// sentido es revertir un archivo que es ENTERO de pruebas y celebrar que nada
/// se rompió.
fn es_archivo_de_pruebas(ruta: &str) -> bool {
    // Las rutas llegan RELATIVAS a la raíz del repositorio, así que hay que
    // cubrir las dos formas: `banco/x.js` en la raíz y `crates/y/banco/x.js`
    // dentro. Lo cazó la prueba con `banco/navegador.js`, que es el banco real
    // de `boi` — escribir solo `contains("/banco/")` lo daba por producción.
    let en_carpeta = |c: &str| {
        ruta.starts_with(&format!("{c}/")) || ruta.contains(&format!("/{c}/"))
    };
    en_carpeta("tests")
        || en_carpeta("banco")
        || ruta.ends_with("_test.rs")
        || ruta.ends_with(".test.js")
}

// ── Código de prueba que vive DENTRO de un archivo de producción ─────────────
//
// `es_archivo_de_pruebas` decide por la ruta, y eso deja fuera el idioma
// dominante de Rust: las pruebas unitarias viven en `#[cfg(test)] mod pruebas`
// dentro del propio `src/`. Aquí ponía que medirlas era legítimo —«se comprueba
// si el resto del banco depende de esa prueba»—, y no sobrevive al uso:
//
//  1. Para una prueba NUEVA ese resultado es cierto POR CONSTRUCCIÓN. Ninguna
//     prueba recién escrita tiene otra que la vigile, así que toda prueba
//     unitaria nueva entraba en la lista de portada. Medido el 2026-08-15 sobre
//     `medico`: de los 4 hunks «SIN CASO ROJO» del informe, **3 eran módulos
//     `#[cfg(test)]` nuevos**. 75 % de falsos positivos en el titular, que es la
//     alarma que a la tercera nadie mira (directiva 23).
//  2. El veredicto dependía de DÓNDE vive el archivo, no de qué es el código:
//     `tests/pantalla.rs` (+305 líneas de prueba) se saltaba por ruta, y el
//     mismo tipo de código en `src/rips/am.rs` salía como hueco de cobertura.
//
// Y el remate: las dos pruebas de ese `am.rs` eran justo las que ponían roja la
// reversión de otro hunk. La herramienta llamó «descubierto» al caso rojo de su
// propio «cubierto».
//
// Con `--incluir-pruebas` se miden igual que antes: la opción no cambia.

/// Comentarios y CONTENIDO de literales, convertidos en espacios; se conservan
/// los saltos de línea y el orden, así que los números de línea del resultado
/// son los del original.
///
/// Sin este paso el detector miente en las dos direcciones, y las dos ocurren de
/// verdad: `#[cfg(feature = "test")]` es producción y contiene la palabra
/// `test`, y una llave dentro de una cadena o de un comentario descuadra el
/// conteo que decide dónde acaba el módulo.
fn limpiar(fuente: &str) -> String {
    enum E {
        Codigo,
        LineaCom,
        BloqueCom(usize),
        Cadena,
        CadenaCruda(usize),
    }
    let cs: Vec<char> = fuente.chars().collect();
    let mut out = String::with_capacity(cs.len());
    let mut e = E::Codigo;
    let mut i = 0;
    // ¿La `r` de `r"…"`/`r#"…"#` abre una cadena cruda, o es parte de un
    // identificador como `otr`? Y `br#"…"#` la lleva detrás de una `b`.
    let abre_cruda = |cs: &[char], i: usize| -> bool {
        let anterior = |k: usize| -> bool {
            k == 0 || !(cs[k - 1].is_alphanumeric() || cs[k - 1] == '_')
        };
        anterior(i) || (cs[i - 1] == 'b' && anterior(i - 1))
    };
    while i < cs.len() {
        let c = cs[i];
        let n = cs.get(i + 1).copied().unwrap_or('\0');
        match e {
            E::Codigo => {
                if c == '/' && n == '/' {
                    e = E::LineaCom;
                    out.push_str("  ");
                    i += 2;
                } else if c == '/' && n == '*' {
                    e = E::BloqueCom(1);
                    out.push_str("  ");
                    i += 2;
                } else if c == 'r' && (n == '"' || n == '#') && abre_cruda(&cs, i) {
                    let mut j = i + 1;
                    let mut h = 0;
                    while cs.get(j) == Some(&'#') {
                        h += 1;
                        j += 1;
                    }
                    if cs.get(j) == Some(&'"') {
                        for _ in i..=j {
                            out.push(' ');
                        }
                        e = E::CadenaCruda(h);
                        i = j + 1;
                    } else {
                        out.push(c);
                        i += 1;
                    }
                } else if c == '"' {
                    e = E::Cadena;
                    out.push('"');
                    i += 1;
                } else if c == '\'' {
                    // Literal de carácter contra TIEMPO DE VIDA: `'{'` cuenta y
                    // `'a` no. Un literal cierra a dos o tres posiciones, o
                    // empieza por barra invertida.
                    let escapado = n == '\\';
                    let corto = cs.get(i + 2) == Some(&'\'');
                    if escapado || corto {
                        let mut j = i + 1;
                        if escapado {
                            j += 1;
                        }
                        while j < cs.len() && cs[j] != '\'' {
                            j += 1;
                        }
                        for c in &cs[i..=j.min(cs.len() - 1)] {
                            out.push(if *c == '\n' { '\n' } else { ' ' });
                        }
                        i = j + 1;
                    } else {
                        out.push(c);
                        i += 1;
                    }
                } else {
                    out.push(c);
                    i += 1;
                }
            }
            E::LineaCom => {
                if c == '\n' {
                    e = E::Codigo;
                    out.push('\n');
                } else {
                    out.push(' ');
                }
                i += 1;
            }
            // Los comentarios de bloque de Rust ANIDAN.
            E::BloqueCom(d) => {
                if c == '/' && n == '*' {
                    e = E::BloqueCom(d + 1);
                    out.push_str("  ");
                    i += 2;
                } else if c == '*' && n == '/' {
                    e = if d == 1 { E::Codigo } else { E::BloqueCom(d - 1) };
                    out.push_str("  ");
                    i += 2;
                } else {
                    out.push(if c == '\n' { '\n' } else { ' ' });
                    i += 1;
                }
            }
            E::Cadena => {
                if c == '\\' {
                    // EL ESCAPE PUEDE SER UNA CONTINUACIÓN DE LÍNEA (`\` + salto),
                    // y este stack la usa a cada rato. Sustituirla por dos
                    // espacios PIERDE un salto, y con él se corren todos los
                    // números de línea de ahí abajo — la región se calcula bien
                    // y apunta al sitio equivocado, que no da ningún error.
                    // Lo cazó el archivo REAL (`vista/rips.rs` de medico): 16
                    // continuaciones, región 16 líneas más arriba de donde está.
                    // Ninguna de las seis pruebas sintéticas lo vio.
                    out.push(' ');
                    out.push(if n == '\n' { '\n' } else { ' ' });
                    i += 2;
                } else if c == '"' {
                    e = E::Codigo;
                    out.push('"');
                    i += 1;
                } else {
                    out.push(if c == '\n' { '\n' } else { ' ' });
                    i += 1;
                }
            }
            E::CadenaCruda(h) => {
                let cierra = c == '"'
                    && (1..=h).all(|k| cs.get(i + k) == Some(&'#'));
                if cierra {
                    e = E::Codigo;
                    for _ in 0..=h {
                        out.push(' ');
                    }
                    i += h + 1;
                } else {
                    out.push(if c == '\n' { '\n' } else { ' ' });
                    i += 1;
                }
            }
        }
    }
    out
}

/// ¿Aparece `palabra` como IDENTIFICADOR entero en `texto`?
fn hay_identificador(texto: &str, palabra: &str) -> bool {
    let t: Vec<char> = texto.chars().collect();
    let p: Vec<char> = palabra.chars().collect();
    let borde = |c: char| !(c.is_alphanumeric() || c == '_');
    t.windows(p.len()).enumerate().any(|(i, w)| {
        w == p.as_slice()
            && (i == 0 || borde(t[i - 1]))
            && (i + p.len() >= t.len() || borde(t[i + p.len()]))
    })
}

/// Las regiones `#[cfg(test)]` del POST-IMAGEN, como rangos de línea 1-based e
/// inclusivos: desde la línea del atributo hasta la que cierra el elemento.
fn regiones_de_prueba(fuente: &str) -> Vec<(usize, usize)> {
    let limpio = limpiar(fuente);
    let cs: Vec<char> = limpio.chars().collect();
    let mut linea_de = vec![1usize; cs.len() + 1];
    let mut ln = 1usize;
    for (i, c) in cs.iter().enumerate() {
        linea_de[i] = ln;
        if *c == '\n' {
            ln += 1;
        }
    }
    linea_de[cs.len()] = ln;

    // Índice del cierre de un grupo abierto en `desde`, contando anidamiento.
    fn cerrar(cs: &[char], desde: usize, abre: char, cierra: char) -> Option<usize> {
        let mut d = 0usize;
        for (i, c) in cs.iter().enumerate().skip(desde) {
            if *c == abre {
                d += 1;
            } else if *c == cierra {
                d -= 1;
                if d == 0 {
                    return Some(i);
                }
            }
        }
        None
    }

    // Dónde acaba el elemento que sigue al atributo: su `}` si tiene cuerpo, o
    // su `;` si no (un `use`, un `struct` unitario).
    fn fin_del_item(cs: &[char], desde: usize) -> Option<usize> {
        let mut i = desde;
        let mut d = 0usize;
        while i < cs.len() {
            let c = cs[i];
            // Otro atributo por delante (`#[cfg(test)] #[derive(…)] struct X;`):
            // se salta entero, o su `[` descuadraría la cuenta.
            if c == '#' {
                let mut j = i + 1;
                while j < cs.len() && cs[j].is_whitespace() {
                    j += 1;
                }
                if cs.get(j) == Some(&'[') {
                    i = cerrar(cs, j, '[', ']')? + 1;
                    continue;
                }
            }
            match c {
                '(' | '[' => d += 1,
                ')' | ']' => d = d.saturating_sub(1),
                '{' if d == 0 => return cerrar(cs, i, '{', '}'),
                ';' if d == 0 => return Some(i),
                _ => {}
            }
            i += 1;
        }
        None
    }

    let mut regiones = Vec::new();
    let mut i = 0;
    while i < cs.len() {
        if cs[i] == '#' {
            let mut j = i + 1;
            // `#![cfg(test)]` es atributo INTERNO —del módulo entero— y aquí no
            // se trata: marcaría el archivo completo como prueba y eso es
            // demasiado para lo que hay medido.
            while j < cs.len() && cs[j].is_whitespace() {
                j += 1;
            }
            if cs.get(j) == Some(&'[')
                && let Some(fin_attr) = cerrar(&cs, j, '[', ']')
            {
                let texto: String = cs[j..=fin_attr].iter().collect();
                // `cfg(test)`, `cfg(all(test, …))` sí; `cfg(not(test))` NO, que
                // es justo lo contrario, y `cfg(feature = "test")` tampoco —su
                // `test` va dentro de una cadena, y `limpiar` ya la vació.
                if hay_identificador(&texto, "cfg")
                    && hay_identificador(&texto, "test")
                    && !hay_identificador(&texto, "not")
                    && let Some(fin) = fin_del_item(&cs, fin_attr + 1)
                {
                    regiones.push((linea_de[i], linea_de[fin]));
                    i = fin + 1;
                    continue;
                }
            }
        }
        i += 1;
    }
    regiones
}

/// Los números de línea del archivo NUEVO que este hunk toca.
///
/// Una línea BORRADA no existe en el post-imagen, así que se le apunta la
/// posición donde estaba. Consecuencia asumida: un hunk que solo BORRA un módulo
/// de pruebas entero no se reconoce como prueba —la región ya no existe donde
/// mirar— y se mide. Se prefiere medir de más a callar de más.
fn lineas_tocadas(h: &Hunk) -> Vec<usize> {
    let mut v = Vec::new();
    let mut n = h.linea;
    let mut en_cuerpo = false;
    for l in h.texto.lines() {
        if !en_cuerpo {
            en_cuerpo = l.starts_with("@@");
            continue;
        }
        // UNA LÍNEA EN BLANCO NO ES CÓDIGO DE PRODUCCIÓN, y por no decirlo se
        // escapó un falso positivo el 2026-08-15: el hunk de
        // `vista/rips.rs` empieza por el renglón vacío que separa el último
        // `fn` del `#[cfg(test)]` de debajo. Esa línea cae FUERA de la región
        // —la región empieza en el atributo— y con ella el hunk dejaba de ser
        // «todo dentro» y volvía a la lista de portada. Un hunk que solo
        // añadiera blancos no llega hasta aquí: `solo_comentarios` ya lo
        // aparta antes.
        let blanca = |l: &str| l[1..].trim().is_empty();
        match l.chars().next() {
            // «\ No newline at end of file» no es una línea del archivo.
            Some('\\') => {}
            Some('+') => {
                if !blanca(l) {
                    v.push(n);
                }
                n += 1;
            }
            Some('-') if !blanca(l) => v.push(n),
            Some('-') => {}
            _ => n += 1,
        }
    }
    v
}

/// ¿TODAS las líneas que este hunk toca caen dentro de una región
/// `#[cfg(test)]`?
///
/// Se exige que caigan todas: un hunk que mezcla producción y prueba se mide,
/// porque lo que interesa es la mitad de producción. La dirección del sesgo es
/// deliberada — clasificar producción como prueba escondería un hueco real,
/// que es el fallo caro (directiva 21).
fn es_hunk_de_prueba(o: &Opciones, h: &Hunk) -> bool {
    if !h.archivo.ends_with(".rs") {
        return false;
    }
    let Ok(fuente) = git(&o.repo, &["show", &format!("{}:{}", o.rama, h.archivo)]) else {
        return false;
    };
    let regiones = regiones_de_prueba(&fuente);
    if regiones.is_empty() {
        return false;
    }
    let tocadas = lineas_tocadas(h);
    !tocadas.is_empty()
        && tocadas
            .iter()
            .all(|n| regiones.iter().any(|(a, b)| n >= a && n <= b))
}

/// Devuelve los hunks medibles y **cuántos archivos del diff quedaron fuera**.
///
/// Lo segundo no es adorno: el barrido se limita a `*.rs` y `*.js`, y hasta el
/// 2026-08-13 eso no se decía en ninguna parte. Un diff que añadiera una guarda
/// de autorización en Python salía del informe sin dejar rastro — y ese informe
/// entra en el prompt de los revisores con la orden de no volver a medir.
fn recoger_hunks(o: &Opciones) -> Result<(Vec<Hunk>, usize), String> {
    // `-U0` NO: con cero líneas de contexto, `git apply -R` de un hunk suelto
    // acierta mucho menos. Tres es el valor por omisión de git y es el que hace
    // que la reversión aplique limpia en la inmensa mayoría de los casos.
    let diff = git(
        &o.repo,
        &["diff", &format!("{}..{}", o.base, o.rama), "--", "*.rs", "*.js"],
    )?;

    let mut hunks = Vec::new();
    let mut archivo_actual = String::new();
    let mut cabecera_archivo = String::new();
    let mut hunk_actual: Option<(usize, String, usize)> = None;

    let cerrar = |hunks: &mut Vec<Hunk>,
                  archivo: &str,
                  cab: &str,
                  actual: Option<(usize, String, usize)>| {
        if let Some((linea, cuerpo, anadidas)) = actual
            && !archivo.is_empty()
        {
            hunks.push(Hunk {
                archivo: archivo.to_string(),
                linea,
                texto: format!("{cab}{cuerpo}"),
                anadidas,
            });
        }
    };

    for l in diff.lines() {
        if l.starts_with("diff --git ") {
            cerrar(&mut hunks, &archivo_actual, &cabecera_archivo, hunk_actual.take());
            // `diff --git a/X b/X` → X
            // `diff --git a/X b/X` → X. Se parte por " b/" y se toma lo último,
            // que aguanta rutas con espacios mejor que trocear por espacios.
            archivo_actual = match l.rsplit_once(" b/") {
                Some((_, ruta)) => ruta.to_string(),
                None => String::new(),
            };
            cabecera_archivo = format!("{l}\n");
        // `hunk_actual.is_none() &&` EN LA CONDICIÓN, no dentro. Estaba dentro,
        // así que estando ya en un hunk la rama se tomaba igual y la línea
        // DESAPARECÍA. Y la colisión es estructural, porque los prefijos de diff
        // son justo `-` y `+`: una línea BORRADA cuyo contenido empiece por
        // `-- ` sale como `--- …` —que en este stack es un comentario SQL en
        // columna 0, con SQLx y SQL empotrado en `.rs`— y una AÑADIDA que
        // empiece por `++ ` sale como `+++ …`.
        //
        // El daño era silencioso por `--recount`: git recalcula los contadores y
        // el parche mutilado APLICA LIMPIO, así que la herramienta emitía un
        // veredicto sobre un hunk que revirtió a medias, contradiciendo su
        // propio contrato. Lo señaló la revisión de seguridad del 2026-08-13.
        } else if hunk_actual.is_none()
            && (l.starts_with("--- ")
                || l.starts_with("+++ ")
                || l.starts_with("index ")
                || l.starts_with("new file mode")
                || l.starts_with("deleted file mode")
                || l.starts_with("similarity index")
                || l.starts_with("rename "))
        {
            cabecera_archivo.push_str(l);
            cabecera_archivo.push('\n');
        } else if l.starts_with("@@") {
            cerrar(&mut hunks, &archivo_actual, &cabecera_archivo, hunk_actual.take());
            let linea = linea_nueva_de(l);
            hunk_actual = Some((linea, format!("{l}\n"), 0));
        } else if let Some((_, cuerpo, anadidas)) = hunk_actual.as_mut() {
            cuerpo.push_str(l);
            cuerpo.push('\n');
            if l.starts_with('+') {
                *anadidas += 1;
            }
        }
    }
    cerrar(&mut hunks, &archivo_actual, &cabecera_archivo, hunk_actual.take());

    if !o.incluir_pruebas {
        hunks.retain(|h| !es_archivo_de_pruebas(&h.archivo));
    }

    // Cuántos archivos del diff NO entraron por su extensión.
    let todos = git(&o.repo, &["diff", &format!("{}..{}", o.base, o.rama), "--name-only"])?;
    let descartados = todos
        .lines()
        .filter(|f| !f.is_empty() && !f.ends_with(".rs") && !f.ends_with(".js"))
        .count();
    Ok((hunks, descartados))
}

/// De `@@ -12,7 +15,9 @@ contexto` saca el `15`.
fn linea_nueva_de(cabecera: &str) -> usize {
    cabecera
        .split('+')
        .nth(1)
        .and_then(|s| s.split([',', ' ']).next())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

/// Restaura el archivo al soltarse, pase lo que pase.
///
/// Va como `Drop` y no como una llamada al final: si `correr_pruebas` entra en
/// pánico, el `?` de más arriba no llega a ejecutarse y el repositorio se
/// quedaría con el hunk revertido.
///
/// **NO cubre un Ctrl-C**, y aquí se decía que sí. Sin manejador de señal,
/// `SIGINT` termina el proceso y `Drop` no corre — medido con testigo el
/// 2026-08-13: la guarda se quedó revertida y `git status` la enseñaba. Es
/// recuperable y semi-ruidoso, pero la promesa estaba escrita en absoluto en
/// dos sitios, que es justo lo que hace que nadie mire.
struct Restaurador<'a> {
    repo: &'a Path,
    archivo: String,
    activo: bool,
}

impl Drop for Restaurador<'_> {
    fn drop(&mut self) {
        if !self.activo {
            return;
        }
        // `checkout --` deshace la modificación de un archivo RASTREADO…
        let _ = Command::new("git")
            .arg("-C")
            .arg(self.repo)
            .args(["checkout", "--", &self.archivo])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        // …y NO BASTA. Revertir un hunk de BORRADO **crea** el archivo, y como
        // en la rama medida no está en el índice, nace NO RASTREADO: fuera del
        // alcance de `checkout --`.
        //
        // Medido el 2026-08-13 con testigo, y es lo peor que hacía esta
        // herramienta: con una rama que RETIRA un `build.rs`, lo resucitaba, su
        // propio `cargo build` LO EJECUTABA, y lo dejaba puesto para que lo
        // ejecutara cada `cargo` posterior del usuario — mientras declaraba el
        // árbol limpio. Un `build.rs` que un commit retiró es justo el caso que
        // la directiva 25 nombra como peligroso: corre en la máquina de cada
        // consumidor.
        //
        // `git clean -f --` ACOTADO a esta ruta, nunca global (directiva 22: la
        // limpieza se acota a lo que la corrida escribió).
        let _ = Command::new("git")
            .arg("-C")
            .arg(self.repo)
            .args(["clean", "-f", "-q", "--", &self.archivo])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// ¿Existe el archivo en la revisión que se mide?
///
/// Si NO existe, este hunk es un BORRADO y revertirlo lo resucita. Eso no se
/// hace, y el motivo se midió con testigo el 2026-08-13: con una rama que
/// retiraba un `build.rs`, la herramienta lo resucitaba y **su propio
/// `cargo build` lo EJECUTABA** — un archivo que un commit quitó, corriendo otra
/// vez en la máquina de quien audita. La directiva 25 nombra `build.rs` como
/// peligroso precisamente porque «se ejecuta en la máquina de cada consumidor».
///
/// Limpiar el residuo después no basta: para entonces ya corrió. La única
/// respuesta correcta es no revertirlo, y decirlo.
fn existe_en_la_rama(o: &Opciones, archivo: &str) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(&o.repo)
        .args(["cat-file", "-e", &format!("{}:{}", o.rama, archivo)])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// ¿Este hunk cambia alguna línea EJECUTABLE?
///
/// Es la misma prueba de inercia que la directiva 25 aplica a un diff entero,
/// bajada al hunk. Sin ella, la herramienta gasta una corrida del banco por cada
/// párrafo de comentario y luego lo denuncia como «sin caso rojo» — que es
/// cierto y es ruido: la prosa no la cubre ninguna prueba, por definición.
///
/// Lo destapó su primera corrida sobre un repositorio de JavaScript el
/// 2026-08-13: de 8 hunks, los 4 «descubiertos» eran los 4 de comentarios. En un
/// taller donde casi todo commit lleva prosa, eso llena el informe de falsos
/// positivos — y una alarma que resulta falsa dos veces es una que a la tercera
/// nadie mira (directiva 23).
///
/// Se mira solo lo que el hunk AÑADE o QUITA, no el contexto.
fn solo_comentarios(texto: &str) -> bool {
    let mut hubo_cambio = false;
    for l in texto.lines() {
        // LAS LÍNEAS DE CABECERA NO SON CAMBIOS, y esto costó una corrida en
        // falso: `h.texto` lleva delante la cabecera del archivo, cuyas líneas
        // `--- a/x` y `+++ b/x` empiezan por `-` y `+` y no son comentarios. Con
        // ellas dentro, NINGÚN hunk se clasificaba nunca como prosa y el filtro
        // no hacía nada.
        //
        // Mi prueba no lo cazó porque le pasaba un hunk PELADO, sin cabecera:
        // el arnés no se parecía a la entrada real (directiva 33).
        if l.starts_with("+++ ") || l.starts_with("--- ") || l.starts_with("diff --git") {
            continue;
        }
        let (marca, resto) = match l.split_at_checked(1) {
            Some(p) => p,
            None => continue,
        };
        if marca != "+" && marca != "-" {
            continue;
        }
        hubo_cambio = true;
        let t = resto.trim();
        // Vacío, comentario de línea, o cuerpo de bloque. Se acepta `*` porque
        // es la continuación de `/* … */`, que en JS es común.
        let es_prosa = t.is_empty()
            || t.starts_with("//")
            || t.starts_with('#')
            || t.starts_with("/*")
            || t.starts_with('*')
            || t.starts_with("*/");
        if !es_prosa {
            return false;
        }
    }
    // Un hunk sin cambios no existe, pero si llegara: no se declara prosa.
    hubo_cambio
}

fn medir_hunk(o: &Opciones, h: &Hunk) -> Result<Veredicto, String> {
    // La prueba de inercia va PRIMERO: no gasta ni una corrida del banco.
    if solo_comentarios(&h.texto) {
        return Ok(Veredicto::SoloComentarios);
    }

    // Y la de código de prueba también va ANTES de tocar el árbol: no gasta una
    // corrida del banco y, sobre todo, no mete una prueba nueva en la lista de
    // portada, donde el resultado es cierto por construcción.
    if !o.incluir_pruebas && es_hunk_de_prueba(o, h) {
        return Ok(Veredicto::CodigoDePrueba);
    }

    // ANTES de tocar nada: si el archivo no está en la rama medida, revertir el
    // hunk lo CREA. Ver `existe_en_la_rama`.
    if !existe_en_la_rama(o, &h.archivo) {
        return Ok(Veredicto::ReviviriaUnBorrado);
    }

    // Se revierte con `git apply -R` alimentando el parche por la entrada
    // estándar: nada de archivos temporales que alguien tenga que limpiar.
    let aplicado = aplicar_reves(&o.repo, &h.texto)?;
    if !aplicado {
        return Ok(Veredicto::NoSePudoRevertir);
    }
    let _guardia = Restaurador {
        repo: &o.repo,
        archivo: h.archivo.clone(),
        activo: true,
    };

    // Con `--sin-compilar` este paso no existe, y entonces el veredicto
    // `NO COMPILA` tampoco: un fallo de compilación se manifestará como banco
    // rojo, o sea como `CUBIERTO`. Se dice en la cabecera para que nadie lo lea
    // como cobertura sin saberlo.
    if !o.sin_compilar && !compila(o)? {
        return Ok(Veredicto::NoCompila);
    }
    if correr_pruebas(o)? {
        // Todo verde con el hunk FUERA: nadie lo estaba mirando.
        return Ok(Veredicto::SinCasoRojo);
    }

    // ROJO CON EL HUNK FUERA. Antes eso era `Cubierto` y punto, y ahí estaba el
    // único camino barato a un VERDE FALSO: la clasificación es por código de
    // salida, así que un banco intermitente —una prueba de tiempo, una que
    // depende de la red, un puerto ocupado— produce el mismo rojo y la
    // herramienta lo lee como «el banco vigila este trozo». Medido por la
    // revisión del 2026-08-13 sobre una guarda que NADIE mide: salió CUBIERTO.
    //
    // La confirmación es la pareja que esta herramienta predica: se restaura el
    // hunk y se vuelve a correr. Si con el hunk PUESTO el banco no vuelve a
    // verde, el rojo no venía del hunk y no hay veredicto.
    //
    // Cuesta una corrida extra SOLO en los hunks que salen cubiertos, que es
    // donde la respuesta importa.
    drop(_guardia);
    if correr_pruebas(o)? {
        Ok(Veredicto::Cubierto)
    } else {
        Ok(Veredicto::BancoInestable)
    }
}

fn aplicar_reves(repo: &Path, parche: &str) -> Result<bool, String> {
    use std::io::Write as _;
    let mut hijo = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["apply", "-R", "--recount", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("could not run git apply: {e}"))?;
    hijo.stdin
        .as_mut()
        .ok_or("git apply did not accept input")?
        .write_all(parche.as_bytes())
        .map_err(|e| format!("could not write the patch: {e}"))?;
    let estado = hijo
        .wait()
        .map_err(|e| format!("git apply did not finish: {e}"))?;
    Ok(estado.success())
}

/// ¿Compila? Se separa de las pruebas porque el veredicto es DISTINTO: un hunk
/// cuya reversión no compila no está medido, y decir «cubierto» ahí sería
/// inventar cobertura.
fn compila(o: &Opciones) -> Result<bool, String> {
    correr(&o.repo, "cargo build --workspace --all-targets")
}

fn correr_pruebas(o: &Opciones) -> Result<bool, String> {
    correr(&o.repo, &o.prueba)
}

fn correr(repo: &Path, orden: &str) -> Result<bool, String> {
    // El shell depende del SO. En Unix es `bash` con `-eo pipefail`, para que una
    // tubería (`cargo test | tee`) falle si falla cualquier eslabón, no solo el
    // último. En Windows `bash` no es el shell del sistema —resolvería al de WSL
    // o a ninguno, y en ambos casos el comando de prueba NUNCA corre y el banco
    // se lee como rojo (el modo de fallo de la migración Linux→Windows)—, así que
    // el shell es `cmd.exe` (COMSPEC): `/C` corre la orden y devuelve el código
    // del último comando. No tiene `pipefail`, pero el comando de prueba típico
    // (`cargo test`) no es tubería, y una tubería en Windows se escribe en
    // sintaxis cmd.
    #[cfg(windows)]
    let mut cmd = {
        let mut c = Command::new("cmd");
        c.args(["/C", orden]);
        c
    };
    #[cfg(not(windows))]
    let mut cmd = {
        let mut c = Command::new("bash");
        c.arg("--noprofile")
            .arg("--norc")
            .args(["-eo", "pipefail", "-c", orden]);
        c
    };
    let estado = cmd
        .current_dir(repo)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("could not run «{orden}»: {e}"))?;
    Ok(estado.success())
}

fn informe(veredictos: &[(Hunk, Veredicto)]) -> String {
    let mut por_veredicto: BTreeMap<&str, Vec<&Hunk>> = BTreeMap::new();
    for (h, v) in veredictos {
        por_veredicto.entry(v.etiqueta()).or_default().push(h);
    }

    let mut s = String::new();
    let _ = writeln!(s, "{}", "=".repeat(72));

    let descubiertos: Vec<&Hunk> = por_veredicto
        .get(Veredicto::SinCasoRojo.etiqueta())
        .cloned()
        .unwrap_or_default();

    // «TODO CUBIERTO» solo si NO queda nada sin medir. La primera versión lo
    // imprimía en cuanto no había descubiertos, y su primera corrida de verdad
    // —sobre esta misma herramienta— sacó el titular verde con los DOS hunks en
    // «NO COMPILA» justo debajo. Es exactamente el fallo que este programa
    // existe para cazar, dentro del programa: un resumen que dice más de lo que
    // se midió (directiva 30).
    let sin_medir: usize = veredictos
        .iter()
        .filter(|(_, v)| {
            matches!(
                v,
                Veredicto::NoCompila
                    | Veredicto::NoSePudoRevertir
                    | Veredicto::BancoInestable
                    | Veredicto::ReviviriaUnBorrado
            )
        })
        .count();

    let prosa = veredictos
        .iter()
        .filter(|(_, v)| *v == Veredicto::SoloComentarios)
        .count();
    let pruebas = veredictos
        .iter()
        .filter(|(_, v)| *v == Veredicto::CodigoDePrueba)
        .count();
    if pruebas > 0 {
        let _ = writeln!(
            s,
            "{pruebas} hunk(s) were TEST CODE inside a production file\n             (`#[cfg(test)]`): not measured and not counted. That no other test\n             watches them is true by construction. With `--include-tests` they are measured."
        );
    }
    if prosa > 0 {
        let _ = writeln!(
            s,
            "{prosa} hunk(s) were COMMENTS ONLY: not measured and not counted. Prose \n\
             is covered by no test, and flagging it would be the alarm nobody looks at."
        );
    }

    if descubiertos.is_empty() && sin_medir == 0 {
        let _ = writeln!(s, "\x1b[1;32mALL COVERED\x1b[0m — reverting each hunk turned something red.");
    } else if descubiertos.is_empty() {
        let _ = writeln!(
            s,
            "\x1b[1;33mINCONCLUSIVE\x1b[0m — none came out uncovered, but {} of {} \
             could not be measured.\n  \
             «Could not be measured» and «is covered» are NOT the same: there is no verdict.",
            sin_medir,
            veredictos.len()
        );
    } else {
        let _ = writeln!(
            s,
            "\x1b[1;33m{} HUNK(S) WITH NO RED TEST\x1b[0m — they can be reverted and the suite doesn't notice:",
            descubiertos.len()
        );
        for h in &descubiertos {
            let _ = writeln!(s, "    · {}:{}  (+{} lines)", h.archivo, h.linea, h.anadidas);
        }
        let _ = writeln!(
            s,
            "\n  Not a defect in itself: there are legitimately unreachable guards.\n  \
             What it must NOT do is go unnoticed — an `ok_or` that never fires is\n  \
             indistinguishable from one wired to the wrong condition (directive 33)."
        );
    }

    for (etiqueta, lista) in &por_veredicto {
        if *etiqueta == Veredicto::SinCasoRojo.etiqueta() {
            continue;
        }
        let _ = writeln!(s, "\n{}: {}", etiqueta, lista.len());
        if *etiqueta != Veredicto::Cubierto.etiqueta() {
            for h in lista {
                let _ = writeln!(s, "    · {}:{}", h.archivo, h.linea);
            }
        }
    }
    let _ = writeln!(s, "{}", "=".repeat(72));
    s
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn la_linea_nueva_sale_de_la_cabecera_del_hunk() {
        assert_eq!(linea_nueva_de("@@ -12,7 +15,9 @@ fn algo() {"), 15);
        assert_eq!(linea_nueva_de("@@ -1 +1 @@"), 1);
        // LA PAREJA: una cabecera que no lo es no inventa un número creíble.
        assert_eq!(linea_nueva_de("no soy una cabecera"), 0);
    }

    #[test]
    fn los_archivos_de_prueba_se_reconocen_por_la_ruta() {
        // La mitad que TIENE que reconocer.
        for r in [
            "crates/x/tests/simulacion.rs",
            "tests/panel_e2e.rs",
            "src/cosa_test.rs",
            "comportamientos/confirmar-doble.test.js",
            "banco/navegador.js",
        ] {
            assert!(es_archivo_de_pruebas(r), "{r} debía reconocerse como prueba");
        }
        // LA PAREJA, y es la que impide un detector que diga que sí a todo:
        // estos son producción y tienen que medirse.
        for r in [
            "crates/x/src/dinero.rs",
            "src/main.rs",
            "boi.js",
            "src/lib/testigo.rs",
        ] {
            assert!(!es_archivo_de_pruebas(r), "{r} NO es archivo de pruebas");
        }
    }

    #[test]
    fn el_veredicto_tiene_cuatro_valores_y_ninguno_se_confunde_con_verde() {
        // «No compila» y «no se pudo revertir» NO son cobertura: son ausencia de
        // medición. Si alguien los colapsara en `Cubierto`, esta prueba se cae.
        assert_ne!(Veredicto::NoCompila.etiqueta(), Veredicto::Cubierto.etiqueta());
        assert_ne!(
            Veredicto::NoSePudoRevertir.etiqueta(),
            Veredicto::Cubierto.etiqueta()
        );
        assert!(Veredicto::NoCompila.etiqueta().contains("unmeasured"));
    }
}

#[cfg(test)]
mod prueba_prosa {
    use super::solo_comentarios;

    /// LA MITAD QUE TIENE QUE RECONOCER la prosa: sin ella, cada párrafo de
    /// comentario gasta una corrida del banco y sale denunciado como
    /// «sin caso rojo», que es cierto y es ruido.
    #[test]
    fn un_hunk_de_puros_comentarios_se_reconoce() {
        let h = "@@ -1,2 +1,4 @@\n context\n+// una explicación\n+//\n+/* bloque */\n- # otro\n";
        assert!(solo_comentarios(h));
    }

    /// **CON LA CABECERA DEL ARCHIVO DELANTE, que es como llega de verdad.**
    ///
    /// La primera versión de esta prueba pasaba un hunk PELADO, y por eso no
    /// cazó que `--- a/x` y `+++ b/x` empiezan por `-` y `+`: con ellas dentro,
    /// ningún hunk se clasificaba jamás como prosa y el filtro entero no hacía
    /// nada. Se vio corriendo la herramienta de verdad sobre `boi`, no aquí
    /// (directiva 33: el arnés tiene que parecerse a la entrada real).
    #[test]
    fn la_cabecera_del_archivo_no_cuenta_como_cambio() {
        let con_cabecera = "diff --git a/boi.js b/boi.js\n\
                            index 111..222 100644\n\
                            --- a/boi.js\n\
                            +++ b/boi.js\n\
                            @@ -1,2 +1,3 @@\n\
                             context\n\
                            +// solo prosa\n";
        assert!(solo_comentarios(con_cabecera), "la cabecera se coló como cambio");

        // LA PAREJA: con la misma cabecera, una línea ejecutable sigue contando.
        let con_codigo = "diff --git a/boi.js b/boi.js\n\
                          --- a/boi.js\n\
                          +++ b/boi.js\n\
                          @@ -1,2 +1,3 @@\n\
                          +  if (x) { return; }\n";
        assert!(!solo_comentarios(con_codigo));
    }

    /// LA PAREJA, y es la que impide un detector que diga «prosa» a todo — que
    /// dejaría de medir el código de verdad sin que se notara.
    #[test]
    fn una_sola_linea_ejecutable_basta_para_medirlo() {
        let h = "@@ -1,2 +1,3 @@\n context\n+// explicación\n+    if n > 100 { return None; }\n";
        assert!(!solo_comentarios(h), "una guarda entre comentarios SÍ se mide");

        // Y el caso que más engaña: quitar código y poner solo comentarios.
        let borra = "@@ -1,2 +1,1 @@\n context\n-    let x = peligro();\n+// se quitó\n";
        assert!(!solo_comentarios(borra), "quitar código no es prosa");
    }

    #[test]
    fn un_hunk_sin_cambios_no_se_declara_prosa() {
        // Solo contexto: no hay nada que decir, y desde luego no «es prosa».
        assert!(!solo_comentarios("@@ -1,1 +1,1 @@\n context\n"));
    }
}

#[cfg(test)]
mod pruebas_de_region_de_prueba {
    use super::{lineas_tocadas, limpiar, regiones_de_prueba, Hunk};

    fn dentro(regiones: &[(usize, usize)], n: usize) -> bool {
        regiones.iter().any(|(a, b)| n >= *a && n <= *b)
    }

    /// LA LISTA QUE SÍ: lo que es región de prueba, y con su pareja dentro —la
    /// línea de producción de arriba NO puede estar dentro—. Sin esa segunda
    /// mitad pasaría igual un detector que devolviera «todo el archivo».
    #[test]
    fn reconoce_el_modulo_de_pruebas_y_deja_fuera_la_produccion() {
        let fuente = "\
pub fn guarda(n: i64) -> Option<i64> {
    if n > 100 { return None; }
    Some(n)
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn la_guarda_rechaza() {
        if true { assert!(guarda(101).is_none()); }
    }
}
";
        let r = regiones_de_prueba(fuente);
        assert_eq!(r.len(), 1, "una sola región: {r:?}");
        assert_eq!(r[0], (6, 14), "del atributo a la llave que cierra: {r:?}");
        assert!(dentro(&r, 11), "el cuerpo de la prueba está dentro");
        // LA PAREJA: la guarda de producción no.
        for n in 1..=5 {
            assert!(!dentro(&r, n), "la línea {n} es producción y quedó dentro: {r:?}");
        }
    }

    /// `#[cfg(test)]` sobre algo SIN cuerpo termina en su `;`, no en la primera
    /// llave que aparezca después —que sería la del módulo siguiente, y se
    /// tragaría medio archivo—.
    #[test]
    fn un_uso_solo_para_pruebas_acaba_en_su_punto_y_coma() {
        let fuente = "\
#[cfg(test)]
use std::collections::HashMap;

pub fn produccion() -> i64 {
    7
}
";
        let r = regiones_de_prueba(fuente);
        assert_eq!(r, vec![(1, 2)], "la región es el `use`, no lo que sigue: {r:?}");
        assert!(!dentro(&r, 5), "la producción de abajo quedó dentro: {r:?}");
    }

    /// LA LISTA QUE NO: tres formas de parecerlo sin serlo. Las tres son
    /// producción y las tres tienen que quedar FUERA — `not(test)` es
    /// literalmente lo contrario, y las otras dos llevan la palabra `test`
    /// dentro de una cadena o de un comentario.
    #[test]
    fn lo_que_parece_prueba_y_no_lo_es_no_se_marca() {
        let casos = [
            ("#[cfg(not(test))]\nfn solo_en_produccion() {\n    let x = 1;\n}\n", "not(test)"),
            ("#[cfg(feature = \"test\")]\nfn con_bandera() {\n    let x = 1;\n}\n", "feature = \"test\""),
            ("// #[cfg(test)]\nfn comentado() {\n    let x = 1;\n}\n", "dentro de un comentario"),
            ("/* #[cfg(test)] mod pruebas { */\nfn en_bloque() {\n    let x = 1;\n}\n", "comentario de bloque"),
        ];
        for (fuente, que) in casos {
            let r = regiones_de_prueba(fuente);
            assert!(r.is_empty(), "«{que}» NO es región de prueba, y salió {r:?}");
        }
    }

    /// Una llave en columna 0 dentro de una CADENA no cierra el módulo.
    ///
    /// Es la razón de que haya un limpiador y no un contador de llaves a pelo:
    /// sin él la región acababa antes de tiempo y las pruebas de después
    /// volvían a contarse como producción, en silencio.
    #[test]
    fn una_llave_dentro_de_una_cadena_no_cierra_el_modulo() {
        let fuente = "\
#[cfg(test)]
mod pruebas {
    #[test]
    fn dibuja() {
        let s = \"linea
}
todavia dentro\";
        assert!(!s.is_empty());
    }
}
pub fn produccion() -> i64 { 1 }
";
        let r = regiones_de_prueba(fuente);
        assert_eq!(r.len(), 1, "{r:?}");
        assert!(dentro(&r, 8), "la línea 8 sigue dentro del módulo: {r:?}");
        assert!(!dentro(&r, 11), "la producción del final NO: {r:?}");
    }

    /// **La continuación de línea de un literal NO puede perder el salto.**
    ///
    /// Es el fallo que las seis pruebas sintéticas no vieron y que cazó el
    /// ARCHIVO REAL el 2026-08-15 (`vista/rips.rs` de medico, 16
    /// continuaciones): la región salía calculada con precisión y apuntando 16
    /// líneas más arriba del módulo. No da ningún error — un rango de líneas
    /// equivocado se ve igual que uno correcto — y por eso la única forma de
    /// verlo fue medir sobre el artefacto, no sobre un fixture.
    ///
    /// La pareja va dentro: el escape que NO es un salto sigue borrándose.
    #[test]
    fn una_continuacion_de_linea_conserva_el_salto() {
        let fuente = "let a = \"uno \\\n         dos\";\n#[cfg(test)]\nmod pruebas {\n}\n";
        assert_eq!(
            limpiar(fuente).lines().count(),
            fuente.lines().count(),
            "el limpiador no puede cambiar el número de líneas"
        );
        let r = regiones_de_prueba(fuente);
        assert_eq!(r, vec![(3, 5)], "la región tiene que caer donde está de verdad: {r:?}");
        // LA PAREJA: un escape normal (`\\n` dentro de la cadena) NO añade línea.
        let otro = "let b = \"uno\\ndos\";\n#[cfg(test)]\nmod p {\n}\n";
        assert_eq!(limpiar(otro).lines().count(), otro.lines().count());
        assert_eq!(regiones_de_prueba(otro), vec![(2, 4)]);
    }

    /// El limpiador, en las dos direcciones: vacía lo que debe y **conserva el
    /// código**. Solo con la primera mitad pasaría uno que lo borrara todo.
    #[test]
    fn el_limpiador_vacia_lo_suyo_y_no_toca_el_codigo() {
        let l = limpiar("let a = \"{}\"; // {\nlet b = 2;\n");
        assert!(!l.contains('{'), "la llave de la cadena y la del comentario se van: {l:?}");
        assert!(l.contains("let a ="), "el código se conserva: {l:?}");
        assert!(l.contains("let b = 2;"), "y la línea siguiente entera: {l:?}");
        assert_eq!(l.lines().count(), 2, "los saltos de línea se conservan: {l:?}");
        // Comentario de bloque ANIDADO: el cierre de dentro no reabre el código.
        let n = limpiar("a /* uno /* dos */ tres */ b\n");
        assert!(!n.contains("dos") && !n.contains("tres"), "{n:?}");
        assert!(n.contains('a') && n.contains('b'), "{n:?}");
    }

    /// Las líneas que un hunk toca son las de `+`/`-`, NO las de contexto: un
    /// hunk que empieza tres líneas antes del `#[cfg(test)]` sigue siendo de
    /// prueba. Es el caso real de `vista/rips.rs` en `medico`.
    #[test]
    fn el_contexto_no_cuenta_como_linea_tocada() {
        let h = Hunk {
            archivo: "src/x.rs".into(),
            linea: 10,
            texto: "diff --git a/src/x.rs b/src/x.rs\n--- a/src/x.rs\n+++ b/src/x.rs\n\
                    @@ -10,3 +10,5 @@ contexto\n contexto1\n contexto2\n+nueva1\n+nueva2\n contexto3\n"
                .into(),
            anadidas: 2,
        };
        assert_eq!(lineas_tocadas(&h), vec![12, 13], "solo las añadidas, y en el archivo NUEVO");
    }

    /// Una línea EN BLANCO añadida no cuenta como línea tocada, y su pareja: una
    /// con código, sí.
    ///
    /// Es el falso positivo que quedó vivo en la primera versión y lo midió la
    /// corrida real del 2026-08-15 sobre `medico`: el hunk de
    /// `vista/rips.rs` añade el renglón vacío que separa el último `fn` del
    /// `#[cfg(test)]`, esa línea cae fuera de la región, y el módulo entero
    /// volvía a la lista de portada. Sin la segunda mitad pasaría igual una
    /// regla que ignorase TODAS las líneas, que es el detector ciego.
    #[test]
    fn una_linea_en_blanco_no_es_linea_tocada_y_una_con_codigo_si() {
        let hunk = |cuerpo: &str| Hunk {
            archivo: "src/x.rs".into(),
            linea: 10,
            texto: format!("diff --git a/src/x.rs b/src/x.rs\n@@ -9,1 +10,3 @@\n contexto\n{cuerpo}"),
            anadidas: 2,
        };
        assert_eq!(
            lineas_tocadas(&hunk("+\n+#[cfg(test)]\n")),
            vec![12],
            "la blanca de la 11 no cuenta; la 12 sí"
        );
        // LA PAREJA: con código dentro, las dos cuentan.
        assert_eq!(
            lineas_tocadas(&hunk("+let x = 1;\n+#[cfg(test)]\n")),
            vec![11, 12],
            "una línea con código NO se ignora nunca"
        );
    }
}

