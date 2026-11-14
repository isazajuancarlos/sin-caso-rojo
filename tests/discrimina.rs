// SPDX-License-Identifier: LicenseRef-Propietario
// SPDX-FileCopyrightText: 2026 Juan Carlos Isaza

//! **La pareja de la herramienta, cruzando su propia frontera.**
//!
//! Un detector que contesta «sin caso rojo» a todo pasa cualquier banco hecho
//! solo de hunks descubiertos, y no se nota: los informes salen llenos y todos
//! dicen lo mismo. Esta prueba monta un repositorio de verdad con **un hunk que
//! TIENE que salir CUBIERTO y otro que TIENE que salir SIN CASO ROJO**, y exige
//! los dos veredictos en la misma corrida.
//!
//! Es la directiva 33 aplicada al instrumento, y la 28: se ejecuta el BINARIO
//! real contra `git` y `cargo` reales, porque lo único que esta herramienta hace
//! es hablar con esos dos procesos y un doble mediría el doble.

use std::path::Path;
use std::process::Command;

fn git(dir: &Path, args: &[&str]) {
    let s = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git");
    assert!(
        s.status.success(),
        "git {:?} falló: {}",
        args,
        String::from_utf8_lossy(&s.stderr)
    );
}

fn escribir(dir: &Path, ruta: &str, contenido: &str) {
    let p = dir.join(ruta);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, contenido).unwrap();
}

/// Un crate mínimo con DOS guardas: una que una prueba mira y otra que no.
fn sembrar(dir: &Path, con_guardas: bool) {
    // Sin esto, `cargo test` deja `target/` y `Cargo.lock` sin rastrear y la
    // cobaya no se parece a un repositorio de verdad.
    escribir(dir, ".gitignore", "/target\n");
    escribir(
        dir,
        "Cargo.toml",
        "[package]\nname = \"cobaya\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[dependencies]\n",
    );
    let (guarda_vista, guarda_ciega) = if con_guardas {
        (
            "    if n > 100 { return None; }\n",
            "    if n > 100 { return None; }\n",
        )
    } else {
        ("", "")
    };
    escribir(
        dir,
        "src/lib.rs",
        // EL RELLENO ENTRE LAS DOS FUNCIONES NO ES ADORNO. `git diff` usa tres
        // líneas de contexto, así que dos cambios a menos de seis líneas caen en
        // el MISMO hunk y esta herramienta —que mide por hunk— no puede
        // separarlos. Lo destapó esta misma prueba: sin el relleno detectaba 1
        // hunk en vez de 2 y no podía dar los dos veredictos.
        //
        // Es una limitación real de la granularidad, y queda dicha aquí porque
        // es lo que hay que saber al leer un informe: dos guardas pegadas se
        // miden juntas, y basta que UNA esté cubierta para que el par salga
        // «cubierto».
        &format!(
            "pub fn vigilada(n: i64) -> Option<i64> {{\n{guarda_vista}    Some(n * 2)\n}}\n\
             \n// relleno para que las dos guardas caigan en hunks distintos\n\
             // 1\n// 2\n// 3\n// 4\n// 5\n// 6\n// 7\n// 8\n\
             \n\
             pub fn ciega(n: i64) -> Option<i64> {{\n{guarda_ciega}    Some(n * 3)\n}}\n\
             \n\
             #[cfg(test)]\n\
             mod pruebas {{\n\
             use super::*;\n\
             #[test]\n\
             fn la_guarda_de_vigilada_se_mira() {{\n\
             assert_eq!(vigilada(101), None);\n\
             assert_eq!(vigilada(2), Some(4));\n\
             }}\n\
             #[test]\n\
             fn de_ciega_solo_se_mira_el_camino_bueno() {{\n\
             assert_eq!(ciega(2), Some(6));\n\
             }}\n\
             }}\n"
        ),
    );
}

#[test]
fn distingue_una_guarda_vigilada_de_una_ciega() {
    let temporal = std::env::temp_dir().join(format!("sin-caso-rojo-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temporal);
    std::fs::create_dir_all(&temporal).unwrap();

    // `-b main`: sin esto git usa `master` y la prueba mediría una rama
    // que no existe — su error diría lo correcto por el motivo equivocado.
    git(&temporal, &["init", "-q", "-b", "main"]);
    git(&temporal, &["config", "user.email", "prueba@local"]);
    git(&temporal, &["config", "user.name", "prueba"]);

    // BASE: sin guardas, y con las dos pruebas ya puestas. Así el único cambio
    // entre los dos commits son las guardas, que es lo que se quiere medir.
    sembrar(&temporal, false);
    git(&temporal, &["add", "-A"]);
    git(&temporal, &["commit", "-qm", "base"]);

    // CABEZA: las dos guardas. Solo una la mira una prueba.
    sembrar(&temporal, true);
    git(&temporal, &["add", "-A"]);
    git(&temporal, &["commit", "-qm", "guardas"]);

    let salida = Command::new(env!("CARGO_BIN_EXE_sin-caso-rojo"))
        .arg(&temporal)
        .args(["--base", "HEAD~1", "--branch", "HEAD", "--test", "cargo test"])
        .output()
        .expect("el binario");
    let texto = String::from_utf8_lossy(&salida.stdout).to_string()
        + &String::from_utf8_lossy(&salida.stderr);

    // LA CORRESPONDENCIA, no las cadenas sueltas. Esto es lo que faltaba, y lo
    // midió la revisión del 2026-08-13 con un mutante: intercambiando los dos
    // veredictos en `medir_hunk`, **el banco entero pasaba en verde**, porque
    // solo se exigía que las palabras «CUBIERTO» y «SIN CASO ROJO» aparecieran
    // en algún sitio de la salida — y con la inversión aparecen las dos igual.
    //
    // Ahora se afirma QUÉ hunk recibió cuál. La guarda ciega vive en `ciega`,
    // que está más abajo en el archivo que `vigilada`, así que su línea es la
    // mayor de las dos: se saca del propio informe en vez de escribirla a mano,
    // que envejecería con el fixture.
    let linea_de = |v: &str| -> Vec<usize> {
        texto
            .lines()
            .filter(|l| l.contains("src/lib.rs:") && l.contains(v))
            .filter_map(|l| {
                l.split("src/lib.rs:")
                    .nth(1)?
                    .split_whitespace()
                    .next()?
                    .parse()
                    .ok()
            })
            .collect()
    };
    let cubiertas = linea_de("COVERED");
    let ciegas = linea_de("NO RED TEST");
    assert_eq!(cubiertas.len(), 1, "se esperaba UN hunk cubierto:\n{texto}");
    assert_eq!(ciegas.len(), 1, "se esperaba UN hunk ciego:\n{texto}");
    assert!(
        cubiertas[0] < ciegas[0],
        "los veredictos están INTERCAMBIADOS: `vigilada` está antes que `ciega` en el \
         archivo, así que el hunk CUBIERTO tiene que ser el de línea menor. \
         cubierto={:?} ciego={:?}\n{texto}",
        cubiertas[0],
        ciegas[0]
    );
    // Y el veredicto agregado: hay descubiertos, así que el código de salida es 1.
    assert_eq!(salida.status.code(), Some(1), "el código de salida no dice que hay descubiertos:\n{texto}");

    // NO DEJA EL REPOSITORIO SUCIO. Es la mitad que nadie mira de una
    // herramienta que edita el código de otro.
    let estado = Command::new("git")
        .arg("-C")
        .arg(&temporal)
        // `--untracked-files=no` por lo mismo que dentro de la herramienta: lo
        // que hay que comprobar es que no dejó ningún archivo RASTREADO
        // modificado. `Cargo.lock` lo crea `cargo test`, no ella.
        .args(["status", "--porcelain", "--untracked-files=no"])
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&estado.stdout).trim().is_empty(),
        "la herramienta dejó archivos rastreados modificados: {}",
        String::from_utf8_lossy(&estado.stdout)
    );

    let _ = std::fs::remove_dir_all(&temporal);
}

/// Y la puerta que su propia corrida de control destapó: con el árbol en otra
/// rama, los parches no casan y el veredicto no valdría. Tiene que negarse.
#[test]
fn se_niega_si_el_arbol_no_esta_en_la_rama_que_se_mide() {
    let temporal = std::env::temp_dir().join(format!("sin-caso-rojo-rama-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temporal);
    std::fs::create_dir_all(&temporal).unwrap();
    // `-b main`: sin esto git usa `master` y la prueba mediría una rama
    // que no existe — su error diría lo correcto por el motivo equivocado.
    git(&temporal, &["init", "-q", "-b", "main"]);
    git(&temporal, &["config", "user.email", "prueba@local"]);
    git(&temporal, &["config", "user.name", "prueba"]);
    sembrar(&temporal, false);
    git(&temporal, &["add", "-A"]);
    git(&temporal, &["commit", "-qm", "base"]);
    sembrar(&temporal, true);
    git(&temporal, &["add", "-A"]);
    git(&temporal, &["commit", "-qm", "guardas"]);
    // El árbol se queda en el commit ANTERIOR al que se le va a pedir medir.
    git(&temporal, &["checkout", "-q", "HEAD~1"]);

    let salida = Command::new(env!("CARGO_BIN_EXE_sin-caso-rojo"))
        .arg(&temporal)
        .args(["--base", "HEAD", "--branch", "main", "--test", "cargo test"])
        .output()
        .expect("el binario");
    let texto = String::from_utf8_lossy(&salida.stderr).to_string();
    assert_eq!(salida.status.code(), Some(2), "debía salir con error:\n{texto}");
    assert!(
        texto.contains("is at") || texto.contains("checkout"),
        "el error no dice cómo arreglarlo:\n{texto}"
    );

    let _ = std::fs::remove_dir_all(&temporal);
}

/// **«No se pudo medir» NO puede leerse como «cubierto».**
///
/// Su primera corrida de verdad —sobre la propia herramienta— imprimió
/// `TODO CUBIERTO` con los dos hunks en `NO COMPILA` justo debajo. Un resumen
/// que dice más de lo que se midió es exactamente el fallo que este programa
/// existe para cazar, dentro del programa (directiva 30).
///
/// El caso: un archivo NUEVO entero. Revertirlo es borrarlo, y entonces nada
/// compila — así que no hay veredicto, y el titular tiene que decirlo.
#[test]
fn un_hunk_que_no_se_pudo_medir_no_sale_como_todo_cubierto() {
    let temporal = std::env::temp_dir().join(format!("scr-nc-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temporal);
    std::fs::create_dir_all(&temporal).unwrap();
    git(&temporal, &["init", "-q", "-b", "main"]);
    git(&temporal, &["config", "user.email", "prueba@local"]);
    git(&temporal, &["config", "user.name", "prueba"]);

    escribir(&temporal, ".gitignore", "/target\n");
    escribir(
        &temporal,
        "Cargo.toml",
        "[package]\nname = \"cobaya\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[dependencies]\n",
    );
    // La prueba va desde la BASE y cubre el cambio de `lib.rs`, para que ese
    // hunk salga CUBIERTO y el único sin medir sea el archivo nuevo. Sin esto,
    // `lib.rs` sale «sin caso rojo» y el titular es ése —correctamente—, así que
    // el caso puro de «no concluyente» no se llegaba a provocar. Lo descubrió
    // esta misma prueba fallando por el motivo equivocado.
    escribir(
        &temporal,
        "src/lib.rs",
        "pub fn a() -> i64 { 1 }\n\
         #[cfg(test)]\n\
         mod t { use super::*; #[test] fn cubre_a() { assert_eq!(a(), 2); } }\n",
    );
    git(&temporal, &["add", "-A"]);
    git(&temporal, &["commit", "-qm", "base"]);

    // Un módulo NUEVO del que el resto ya depende: quitarlo no compila.
    escribir(&temporal, "src/nuevo.rs", "pub fn b() -> i64 { 2 }\n");
    escribir(
        &temporal,
        "src/lib.rs",
        "pub mod nuevo;\n\
         pub fn a() -> i64 { nuevo::b() }\n\
         #[cfg(test)]\n\
         mod t { use super::*; #[test] fn cubre_a() { assert_eq!(a(), 2); } }\n",
    );
    git(&temporal, &["add", "-A"]);
    git(&temporal, &["commit", "-qm", "nuevo"]);

    let salida = Command::new(env!("CARGO_BIN_EXE_sin-caso-rojo"))
        .arg(&temporal)
        .args(["--base", "HEAD~1", "--branch", "HEAD", "--test", "cargo test"])
        .output()
        .expect("el binario");
    let texto = String::from_utf8_lossy(&salida.stdout).to_string();

    assert!(
        !texto.contains("ALL COVERED"),
        "dijo ALL COVERED sin haber medido:\n{texto}"
    );
    assert!(
        texto.contains("INCONCLUSIVE"),
        "no dijo que el resultado no concluye:\n{texto}"
    );
    // Y el código de salida lo dice también, para el guion que solo mire `rc`.
    assert_eq!(
        salida.status.code(),
        Some(3),
        "el código de salida no distingue «no concluyente» de «verde»:\n{texto}"
    );

    let _ = std::fs::remove_dir_all(&temporal);
}

/// **Revertir un borrado RESUCITA el archivo y lo EJECUTA. Con testigo.**
///
/// El agujero peor que tuvo esta herramienta, encontrado por una revisión de
/// seguridad el 2026-08-13 y reproducido aquí: con una rama que RETIRA un
/// `build.rs`, la reversión lo devolvía al árbol y el `cargo build` de la propia
/// herramienta **lo ejecutaba** — un archivo que un commit quitó, corriendo otra
/// vez en la máquina de quien audita, y quedándose puesto para el `cargo`
/// siguiente. La directiva 25 nombra `build.rs` como peligroso justamente
/// porque «se ejecuta en la máquina de cada consumidor».
///
/// Limpiar el residuo después NO basta: para entonces ya corrió. Por eso el
/// arreglo es no revertirlo, y esta prueba lo fija con la única evidencia que
/// no admite discusión — un archivo testigo que el `build.rs` escribe.
#[test]
fn no_resucita_un_borrado_ni_lo_ejecuta() {
    let temporal = std::env::temp_dir().join(format!("scr-rev-{}", std::process::id()));
    let testigo = std::env::temp_dir().join(format!("scr-testigo-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temporal);
    let _ = std::fs::remove_file(&testigo);
    std::fs::create_dir_all(&temporal).unwrap();
    git(&temporal, &["init", "-q", "-b", "main"]);
    git(&temporal, &["config", "user.email", "prueba@local"]);
    git(&temporal, &["config", "user.name", "prueba"]);

    escribir(&temporal, ".gitignore", "/target\n");
    escribir(
        &temporal,
        "Cargo.toml",
        "[package]\nname = \"cobaya\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    escribir(&temporal, "src/lib.rs", "pub fn a() -> i64 { 1 }\n");
    escribir(
        &temporal,
        "build.rs",
        &format!(
            "fn main() {{ std::fs::write({:?}, \"corrio\").ok(); }}\n",
            testigo.display().to_string()
        ),
    );
    git(&temporal, &["add", "-A"]);
    git(&temporal, &["commit", "-qm", "con build.rs"]);

    // La rama que se mide RETIRA el `build.rs`.
    git(&temporal, &["rm", "-q", "build.rs"]);
    git(&temporal, &["commit", "-qm", "quitar build.rs"]);

    let _ = std::fs::remove_file(&testigo);
    let salida = Command::new(env!("CARGO_BIN_EXE_sin-caso-rojo"))
        .arg(&temporal)
        .args(["--base", "HEAD~1", "--branch", "HEAD", "--test", "cargo test"])
        .output()
        .expect("el binario");
    let texto = String::from_utf8_lossy(&salida.stdout).to_string();

    // LO QUE NO PUEDE PASAR, y es la aserción entera de esta prueba.
    assert!(
        !testigo.exists(),
        "EL `build.rs` RETIRADO SE EJECUTÓ durante la medición:\n{texto}"
    );
    // Y no queda puesto en el árbol.
    let estado = Command::new("git")
        .arg("-C")
        .arg(&temporal)
        .args(["status", "--porcelain"])
        .output()
        .unwrap();
    let est = String::from_utf8_lossy(&estado.stdout);
    assert!(
        !est.contains("build.rs"),
        "el `build.rs` retirado quedó en el árbol: {est}"
    );
    // Y se DICE, en vez de contarse como medido.
    assert!(
        texto.contains("WOULD REVIVE A DELETION"),
        "no dijo por qué no lo midió:\n{texto}"
    );
    assert_eq!(salida.status.code(), Some(3), "no lo marcó como no concluyente:\n{texto}");

    let _ = std::fs::remove_dir_all(&temporal);
    let _ = std::fs::remove_file(&testigo);
}

/// **Una prueba nueva NO es un hueco de cobertura**, y su pareja: la guarda de
/// producción que la acompaña SÍ lo es, en la misma corrida.
///
/// Es la pareja de la directiva 33 sobre el cambio del 2026-08-15. Las dos
/// mitades hacen falta: solo con la primera pasaría un detector que llamara
/// «PRUEBA» a todo —y entonces la herramienta no encontraría nada nunca—; solo
/// con la segunda, el estado del que venimos, en el que toda prueba unitaria
/// nueva entraba en la lista de portada.
///
/// El mutante ya existía y es la versión anterior del programa: sin el filtro
/// por región `#[cfg(test)]`, el hunk del módulo de pruebas salía como
/// `SIN CASO ROJO` y la primera afirmación se pone roja.
#[test]
fn una_prueba_nueva_no_cuenta_como_hueco_y_la_guarda_si() {
    let temporal =
        std::env::temp_dir().join(format!("sin-caso-rojo-region-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temporal);
    std::fs::create_dir_all(&temporal).unwrap();

    git(&temporal, &["init", "-q", "-b", "main"]);
    git(&temporal, &["config", "user.email", "prueba@local"]);
    git(&temporal, &["config", "user.name", "prueba"]);

    escribir(&temporal, ".gitignore", "/target\n");
    escribir(
        &temporal,
        "Cargo.toml",
        "[package]\nname = \"cobaya\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[dependencies]\n",
    );

    // BASE: una función sin guarda y un módulo de pruebas que ya existe —así el
    // diff no CREA el módulo entero, que es el caso realista: se le AÑADE una
    // prueba—. El marcador es lo que sustituye la cabeza; el `\` de continuación
    // de Rust se come los espacios de la línea siguiente, así que buscar por
    // indentación no casa (probado: no casaba y el fixture producía UN solo hunk).
    const MARCADOR: &str = "// aqui van mas pruebas\n";
    let base = format!(
        "pub fn ciega(n: i64) -> Option<i64> {{\n\
             Some(n * 3)\n\
         }}\n\
         \n// relleno para separar los dos hunks\n\
         // 1\n// 2\n// 3\n// 4\n// 5\n// 6\n// 7\n// 8\n\
         \n\
         #[cfg(test)]\n\
         mod pruebas {{\n\
         use super::*;\n\
         #[test]\n\
         fn el_camino_bueno() {{\n\
         assert_eq!(ciega(2), Some(6));\n\
         }}\n\
         {MARCADOR}\
         }}\n"
    );
    escribir(&temporal, "src/lib.rs", &base);
    git(&temporal, &["add", "-A"]);
    git(&temporal, &["commit", "-qm", "base"]);

    // CABEZA: dos cambios en el MISMO archivo y en hunks distintos —una guarda
    // de producción que nadie mira, y una prueba nueva dentro del `#[cfg(test)]`
    // que no habla de la guarda—.
    let cabeza = base
        .replace(
            "pub fn ciega(n: i64) -> Option<i64> {\n",
            "pub fn ciega(n: i64) -> Option<i64> {\nif n > 100 { return None; }\n",
        )
        .replace(
            MARCADOR,
            "#[test]\nfn otra_cosa_del_camino_bueno() {\nassert_eq!(ciega(3), Some(9));\n}\n",
        );
    assert!(
        cabeza.contains("if n > 100") && cabeza.contains("otra_cosa_del_camino_bueno"),
        "el fixture tiene que llevar LOS DOS cambios:\n{cabeza}"
    );
    escribir(&temporal, "src/lib.rs", &cabeza);
    git(&temporal, &["add", "-A"]);
    git(&temporal, &["commit", "-qm", "guarda + prueba"]);

    let salida = Command::new(env!("CARGO_BIN_EXE_sin-caso-rojo"))
        .arg(&temporal)
        .args(["--base", "HEAD~1", "--branch", "HEAD", "--test", "cargo test"])
        .output()
        .expect("el binario");
    let texto = String::from_utf8_lossy(&salida.stdout).to_string()
        + &String::from_utf8_lossy(&salida.stderr);

    assert!(
        texto.contains("TEST (n/a)"),
        "la prueba nueva tiene que salir en su renglón propio:\n{texto}"
    );
    // LA PAREJA: la guarda de producción sigue denunciándose. Sin esto, un
    // detector que lo clasificara TODO como prueba pasaría esta prueba.
    assert!(
        texto.contains("NO RED TEST"),
        "la guarda ciega sigue siendo un hueco y tiene que denunciarse:\n{texto}"
    );
    // Y el titular cuenta UNO, no dos: es el número que se lee de un vistazo.
    assert!(
        texto.contains("1 HUNK(S) WITH NO RED TEST"),
        "el titular tiene que contar solo la guarda:\n{texto}"
    );

    let _ = std::fs::remove_dir_all(&temporal);
}

/// **CONTRATO BILINGÜE: los alias en español TIENEN que seguir funcionando.**
///
/// La herramienta se vende en inglés —flags primarias en inglés, `--help` en
/// inglés—, pero nuestras skills la invocan con `--rama`/`--prueba`. Si eso se
/// rompe, los invocadores internos fallan en silencio. Esta prueba corre el
/// binario DOS veces sobre el MISMO repositorio —una con flags inglesas, otra
/// con los alias españoles— y exige salida idéntica. Es la directiva 33: el
/// control (inglés) y el caso que de verdad hay que proteger (español) en la
/// misma corrida, por la vía real (el binario, no un doble).
#[test]
fn los_alias_en_espanol_siguen_funcionando() {
    let temporal =
        std::env::temp_dir().join(format!("sin-caso-rojo-alias-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temporal);
    std::fs::create_dir_all(&temporal).unwrap();

    git(&temporal, &["init", "-q", "-b", "main"]);
    git(&temporal, &["config", "user.email", "prueba@local"]);
    git(&temporal, &["config", "user.name", "prueba"]);

    sembrar(&temporal, false);
    git(&temporal, &["add", "-A"]);
    git(&temporal, &["commit", "-qm", "base"]);
    sembrar(&temporal, true);
    git(&temporal, &["add", "-A"]);
    git(&temporal, &["commit", "-qm", "guardas"]);

    let correr = |flag_rama: &str, flag_prueba: &str| -> (Option<i32>, String) {
        let salida = Command::new(env!("CARGO_BIN_EXE_sin-caso-rojo"))
            .arg(&temporal)
            .args(["--base", "HEAD~1", flag_rama, "HEAD", flag_prueba, "cargo test"])
            .output()
            .expect("el binario");
        (
            salida.status.code(),
            String::from_utf8_lossy(&salida.stdout).to_string()
                + &String::from_utf8_lossy(&salida.stderr),
        )
    };

    let (rc_en, _) = correr("--branch", "--test");
    let (rc_es, texto_es) = correr("--rama", "--prueba");

    // El alias español produce EXACTAMENTE lo mismo que la flag inglesa.
    assert_eq!(
        rc_en, rc_es,
        "el alias español dio un código de salida distinto al de la flag inglesa"
    );
    assert_eq!(rc_es, Some(1), "con los alias españoles no midió:\n{texto_es}");
    // Y los veredictos (en inglés) aparecen: el alias no cambia la salida, solo
    // se acepta en la entrada.
    assert!(
        texto_es.contains("COVERED") && texto_es.contains("NO RED TEST"),
        "la corrida con alias españoles no produjo los veredictos:\n{texto_es}"
    );

    // Y `--help` muestra SOLO inglés: ni rastro de los alias españoles en la ayuda.
    let ayuda = Command::new(env!("CARGO_BIN_EXE_sin-caso-rojo"))
        .arg("--help")
        .output()
        .expect("el binario");
    let texto_ayuda = String::from_utf8_lossy(&ayuda.stdout).to_string();
    assert!(texto_ayuda.contains("--branch"), "la ayuda debe mostrar --branch:\n{texto_ayuda}");
    assert!(
        !texto_ayuda.contains("--rama") && !texto_ayuda.contains("--prueba"),
        "la ayuda NO puede mostrar los alias españoles:\n{texto_ayuda}"
    );

    let _ = std::fs::remove_dir_all(&temporal);
}
