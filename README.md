<!--
SPDX-License-Identifier: LicenseRef-Propietario
SPDX-FileCopyrightText: 2026 Juan Carlos Isaza Arenas
-->

# sin-caso-rojo

> Diff coverage gap finder — which parts of a change does the test suite not *actually* cover?

**[🇬🇧 English](#english) · [🇪🇸 Español](#español)**

---

## English
<a name="english"></a>

**Which parts of this change does the test suite not cover?**

```bash
cargo build --release --manifest-path /mnt/data/conocimiento/sin-caso-rojo/Cargo.toml

# Rust (default)
/mnt/data/conocimiento/sin-caso-rojo/target/release/sin-caso-rojo \
    /mnt/data/inventario --base main --branch my-branch

# JavaScript or anything that is not cargo
/mnt/data/conocimiento/sin-caso-rojo/target/release/sin-caso-rojo \
    /mnt/data/boi --base main --branch my-branch \
    --no-build --test "node --test boi.test.js"
```

For each hunk the diff adds in production code: it reverts it —only that one—,
runs the tests, restores it, and says whether anything turned red.

| verdict | what it means |
|---|---|
| **COVERED** | reverting it turned something red. The suite watches that part |
| **NO RED TEST** | reverting it, everything stayed green. Nobody measures that part |
| **DOES NOT COMPILE (unmeasured)** | reverting it alone breaks the build: it cannot be known |
| **COULD NOT REVERT** | the patch did not match the tree. Not coverage either |
| **WOULD REVIVE A DELETION** | the hunk deletes a file; reverting it resurrects and runs it. Not touched |
| **FLAKY SUITE** | it turned red on revert **and stayed red on restore**: the red did not come from the hunk |
| **COMMENTS ONLY** | the hunk changes not a single executable line. Not measured and not counted |
| **TEST (n/a)** | the hunk falls entirely inside a `#[cfg(test)]` region. It is test code inside a production file: that no other test watches it is true by construction. Measured with `--include-tests` |

The last ones exist because "could not be measured" and "is covered" are **not
the same thing**, and collapsing them would be the very failure this tool hunts
for (directive 30).

**`TEST (n/a)` arrived on 2026-08-15, and because of a measurement.** Until that
day test code was decided only by PATH —`tests/`, `banco/`, `*_test.rs`—, and
that leaves out Rust's dominant idiom: unit tests live in
`#[cfg(test)] mod pruebas` inside `src/` itself. On a `medico` branch,
**3 of the 4 hunks under the "NO RED TEST" headline were new test modules**:
75% false positives in the first line of the report, which is the alarm that by
the third time nobody looks at (directive 23). For a NEW test the result is true
by construction —no freshly written test has another one watching it—, so it
says nothing; and the verdict depended on where the file lived rather than on
what the code was. The kicker: the two tests in that `am.rs` were exactly the
ones that turned red the revert of another hunk, i.e. the tool called
"discovered" the red case of its own "covered".

The region is computed over the **post-image** (`git show <branch>:<file>`) with
a cleaner that empties comments and literal contents before counting braces:
without it, `#[cfg(feature = "test")]` would pass for a test and a brace inside a
string would close the module too early. A hunk that MIXES production and test is
measured — classifying production as test would hide a real gap, and that is the
expensive error (directive 21).

Output: `0` all covered **and all measured** · `1` there are hunks with no red
test · `2` nothing could be measured · `3` **inconclusive** (none discovered, but
some were left unmeasured).

The `3` was not in the first version, and its absence produced the failure on the
first real run: the tool printed **`ALL COVERED`** with the two hunks in
`DOES NOT COMPILE` right below. A summary that says more than what was measured is
exactly the defect this program exists to hunt, inside the program itself. It is
fixed by `un_hunk_que_no_se_pudo_medir_no_sale_como_todo_cubierto`.

### Flags are bilingual

The primary flag names are English and `--help` shows only English. For
backward compatibility with internal callers, the Spanish aliases are still
accepted on input:

| primary (English) | accepted alias |
|---|---|
| `--base` | `--base` |
| `--branch` | `--rama` |
| `--test` | `--prueba` |
| `--include-tests` | `--incluir-pruebas` |
| `--no-build` | `--sin-compilar` |
| `--list` | `--lista` |

### Why it exists, with its measurement

On 2026-08-13, a security review of `inventario` found that of the six new guards
in a commit —six `ok_or`/`checked_add` that reject an impossible amount—
**none had a red test**. It proved it by reverting them by hand and watching the
suite stay at 103/103 green, with its positive control: breaking the *result*
instead of the guard turned 7 tests red, so the suite did execute those lines.

That cost a whole pass of a large model reasoning, editing and running
`cargo test`. **And it does not need a model: it is deterministic.**

This tool does that part. The model is left with what does require judgment:
deciding whether a discovered hunk matters.

**What was verified the day it was born**: a run over that same range
(`95b1a00..44a3ffd` of `inventario`) reproduced the finding on its own, naming
the same files the human reviewer did.

### What it is not

It is not a reviewer. It does not opine, does not look for vulnerabilities and
does not read the code. It produces the **residue** on which someone has to
decide.

A "no red test" hunk **is not a defect in itself**: there are legitimately
unreachable guards —right now, in `inventario`, the one for the applied total of
a receipt is one, and it is written in the code—. What it cannot do is go
unnoticed.

### How it fits with `revisar-seguridad`

The `revisar-seguridad` skill dispatches reviewers to fresh contexts. This tool
goes **before**: its report enters the prompt of those reviewers, so they do not
spend turns figuring out what a program answers for free.

Suggested order:

1. `diff_para_revisar.py` — collects the right diff and refuses to lie.
2. `sin-caso-rojo` — says what of that diff nobody measures.
3. `revisar-seguridad` — the reviewers, with both things already in hand.

### Limits, and you need to know them when reading a report

- **It measures by HUNK, not by line.** `git diff` uses three lines of context,
  so two guards less than six lines apart fall in the same hunk and are measured
  together: it is enough for one to be covered for the pair to come out
  "covered". Its own test uncovered it: without padding between the two functions
  it detected 1 hunk instead of 2.
- **The tree has to be on `--branch`**, or the patches do not match. It refuses
  rather than doing `checkout` on its own: switching someone else's repository to
  another branch is one of those things that do not undo if something goes wrong
  midway. Its first control run uncovered it, where 5 of 13 hunks came out "could
  not revert".
- **It costs one suite run per hunk.** With `--test` you can narrow it
  (`cargo test -p a-crate`); the tool prints the estimate before starting.
- **It does not measure test files** unless `--include-tests` is given: reverting
  a test and seeing nothing break says nothing.
- **A whole new file cannot be measured**: reverting it is deleting it and
  nothing compiles, so it comes out `DOES NOT COMPILE (unmeasured)` —correctly. It
  is the normal case when introducing a module, and that is why the aggregate
  verdict of such a change is `INCONCLUSIVE` and not green.
- **It does not measure DELETION hunks**, and it is a measured security decision:
  reverting a deletion resurrects the file, and if it is a `build.rs` the tool
  itself EXECUTES it when compiling. Cleaning up afterwards is not enough — by
  then it already ran.
- **A `COVERED` costs two runs**: when it turns red the hunk is restored and run
  again. Without that, a flaky suite turns "nobody measures it" into "covered",
  which is the only direction in which this tool can lie dangerously.
- **A Ctrl-C leaves the hunk reverted**: `Drop` does not run on `SIGINT`. The
  program prints the recovery command before starting.
- **In a repository with files that some hooks rewrite** —`conocimiento` is one—
  it refuses to measure, because the tree is never clean. The way out is to clone
  to a temporary and measure there; it is not forced, and it is stated in the
  report.

### Its own pair

`tests/discrimina.rs` sets up a real repository with **one guard a test watches
and another it does not**, and demands BOTH verdicts in the same run. A detector
that answered "no red test" to everything would pass a suite made only of
discovered cases without it being noticed. It also guards the bilingual contract:
the Spanish aliases must produce output identical to the English flags.

```bash
cargo test --manifest-path /mnt/data/conocimiento/sin-caso-rojo/Cargo.toml
```

---

## Español
<a name="español"></a>

**¿Qué trozos de este cambio no cubre el banco de pruebas?**

```bash
cargo build --release --manifest-path /mnt/data/conocimiento/sin-caso-rojo/Cargo.toml

# Rust (por omisión)
/mnt/data/conocimiento/sin-caso-rojo/target/release/sin-caso-rojo \
    /mnt/data/inventario --base main --branch mi-rama

# JavaScript u otro que no sea cargo
/mnt/data/conocimiento/sin-caso-rojo/target/release/sin-caso-rojo \
    /mnt/data/boi --base main --branch mi-rama \
    --no-build --test "node --test boi.test.js"
```

Para cada hunk que el diff añade en código de producción: lo revierte —solo
ése—, corre las pruebas, lo restaura, y dice si algo se puso rojo.

| veredicto | qué significa |
|---|---|
| **COVERED** | al revertirlo, algo se puso rojo. El banco vigila ese trozo |
| **NO RED TEST** | al revertirlo, todo siguió verde. Nadie mide ese trozo |
| **DOES NOT COMPILE (unmeasured)** | revertirlo solo rompe la compilación: no se puede saber |
| **COULD NOT REVERT** | el parche no casó con el árbol. Tampoco es cobertura |
| **WOULD REVIVE A DELETION** | el hunk borra un archivo; revertirlo lo resucita y lo ejecuta. No se toca |
| **FLAKY SUITE** | se puso rojo al revertir **y siguió rojo al restaurar**: el rojo no venía del hunk |
| **COMMENTS ONLY** | el hunk no cambia una sola línea ejecutable. No se mide y no cuenta |
| **TEST (n/a)** | el hunk cae entero en una región `#[cfg(test)]`. Es código de prueba dentro de un archivo de producción: que ninguna otra prueba lo vigile es cierto por construcción. Se mide con `--include-tests` |

Los dos últimos existen porque «no se pudo medir» y «está cubierto» **no son lo
mismo**, y colapsarlos sería el fallo que esta herramienta caza (directiva 30).

**`TEST (n/a)` entró el 2026-08-15, y por una medición.** Hasta ese día lo de
prueba se decidía solo por la RUTA —`tests/`, `banco/`, `*_test.rs`—, y eso deja
fuera el idioma dominante de Rust: las unitarias viven en
`#[cfg(test)] mod pruebas` dentro del propio `src/`. Sobre una rama de `medico`,
**3 de los 4 hunks del titular «NO RED TEST» eran módulos de prueba nuevos**:
75 % de falsos positivos en la primera línea del informe, que es la alarma que a
la tercera nadie mira (directiva 23). Para una prueba NUEVA el resultado es
cierto por construcción —ninguna prueba recién escrita tiene otra que la
vigile—, así que no dice nada; y el veredicto dependía de dónde vivía el archivo
en vez de qué era el código. El remate: las dos pruebas de ese `am.rs` eran
justo las que ponían roja la reversión de otro hunk, o sea que la herramienta
llamó «descubierto» al caso rojo de su propio «cubierto».

La región se calcula sobre el **post-imagen** (`git show <rama>:<archivo>`) con
un limpiador que vacía comentarios y contenido de literales antes de contar
llaves: sin él, `#[cfg(feature = "test")]` pasaría por prueba y una llave dentro
de una cadena cerraría el módulo antes de tiempo. Un hunk que MEZCLA producción
y prueba se mide — clasificar producción como prueba escondería un hueco real,
y ése es el error caro (directiva 21).

Salida: `0` todo cubierto **y todo medido** · `1` hay hunks sin caso rojo ·
`2` no se pudo medir nada · `3` **no concluyente** (ninguno descubierto, pero
alguno quedó sin medir).

El `3` no estaba en la primera versión, y su ausencia produjo el fallo en la
primera corrida de verdad: la herramienta imprimió **`ALL COVERED`** con los dos
hunks en `DOES NOT COMPILE` justo debajo. Un resumen que dice más de lo que se
midió es exactamente el defecto que este programa existe para cazar, dentro del
programa. Lo fija `un_hunk_que_no_se_pudo_medir_no_sale_como_todo_cubierto`.

### Las banderas son bilingües

Los nombres primarios de las banderas están en inglés y `--help` muestra solo
inglés. Por compatibilidad con los llamantes internos, los alias en español se
siguen aceptando en la entrada:

| primaria (inglés) | alias aceptado |
|---|---|
| `--base` | `--base` |
| `--branch` | `--rama` |
| `--test` | `--prueba` |
| `--include-tests` | `--incluir-pruebas` |
| `--no-build` | `--sin-compilar` |
| `--list` | `--lista` |

### Por qué existe, con su medición

El 2026-08-13, una revisión de seguridad de `inventario` encontró que de las
seis guardas nuevas de un commit —seis `ok_or`/`checked_add` que rechazan un
importe imposible— **ninguna tenía caso rojo**. Lo demostró revirtiéndolas a
mano y viendo la suite quedarse en 103/103 verde, con su positivo de control:
romper el *resultado* en vez de la guarda ponía 7 pruebas rojas, así que la
suite sí ejecutaba esas líneas.

Eso costó una pasada entera de un modelo grande razonando, editando y corriendo
`cargo test`. **Y no necesita un modelo: es determinista.**

Esta herramienta hace esa parte. El modelo se queda con lo que sí requiere
criterio: decidir si un hunk descubierto importa.

**Lo comprobado el día que nació**: corrida sobre ese mismo rango
(`95b1a00..44a3ffd` de `inventario`) reprodujo el hallazgo por su cuenta,
nombrando los mismos archivos que el revisor humano.

### Lo que NO es

No es un revisor. No opina, no busca vulnerabilidades y no lee el código.
Produce el **residuo** sobre el que hay que decidir.

Un hunk «sin caso rojo» **no es un defecto por sí mismo**: hay guardas
legítimamente inalcanzables —hoy mismo, en `inventario`, la del total aplicado
de un recibo lo es, y está escrito en el código—. Lo que no puede es pasar
inadvertido.

### Cómo encaja con `revisar-seguridad`

La skill `revisar-seguridad` despacha revisores a contextos frescos. Esta
herramienta va **antes**: su informe entra en el prompt de esos revisores, que
así no gastan turnos averiguando lo que un programa contesta gratis.

Orden sugerido:

1. `diff_para_revisar.py` — recoge el diff correcto y se niega a mentir.
2. `sin-caso-rojo` — dice qué de ese diff no mide nadie.
3. `revisar-seguridad` — los revisores, con las dos cosas ya en la mano.

### Límites, y hay que saberlos al leer un informe

- **Mide por HUNK, no por línea.** `git diff` usa tres líneas de contexto, así
  que dos guardas a menos de seis líneas caen en el mismo hunk y se miden
  juntas: basta que una esté cubierta para que el par salga «cubierto». Lo
  destapó su propia prueba, que sin relleno entre las dos funciones detectaba 1
  hunk en vez de 2.
- **El árbol tiene que estar en `--branch`**, o los parches no casan. Se niega en
  vez de hacer `checkout` por su cuenta: cambiar de rama el repositorio de otro
  es de las cosas que no se deshacen si algo sale a mitad. Lo destapó su primera
  corrida de control, donde 5 de 13 hunks salieron «no se pudo revertir».
- **Cuesta una corrida del banco por hunk.** Con `--test` se puede acotar
  (`cargo test -p un-crate`); la herramienta imprime la estimación antes de
  empezar.
- **No mide archivos de prueba** salvo con `--include-tests`: revertir una
  prueba y ver que nada se rompe no dice nada.
- **Un archivo NUEVO entero no se puede medir**: revertirlo es borrarlo y nada
  compila, así que sale `DOES NOT COMPILE (unmeasured)` — correctamente. Es el
  caso normal al estrenar un módulo, y por eso el veredicto agregado de un cambio
  así es `INCONCLUSIVE` y no verde.
- **No mide hunks de BORRADO**, y es una decisión de seguridad medida: revertir
  un borrado resucita el archivo, y si es un `build.rs` la propia herramienta lo
  EJECUTA al compilar. Limpiar después no basta — para entonces ya corrió.
- **Un `COVERED` cuesta dos corridas**: al ponerse rojo se restaura el hunk y
  se vuelve a correr. Sin eso, un banco intermitente convierte «nadie lo mide»
  en «cubierto», que es la única dirección en la que esta herramienta puede
  mentir peligrosamente.
- **Un Ctrl-C deja el hunk revertido**: `Drop` no corre ante `SIGINT`. El
  programa imprime el comando de recuperación antes de empezar.
- **En un repositorio con archivos que unos hooks reescriben** —`conocimiento`
  es uno— se niega a medir, porque el árbol nunca está limpio. La salida es
  clonar a un temporal y medir ahí; no se fuerza, y queda dicho en el informe.

### Su propia pareja

`tests/discrimina.rs` monta un repositorio de verdad con **una guarda que una
prueba mira y otra que no**, y exige los DOS veredictos en la misma corrida. Un
detector que contestara «sin caso rojo» a todo pasaría un banco hecho solo de
casos descubiertos sin que se notara. También protege el contrato bilingüe: los
alias en español deben producir una salida idéntica a la de las banderas en
inglés.

```bash
cargo test --manifest-path /mnt/data/conocimiento/sin-caso-rojo/Cargo.toml
```
