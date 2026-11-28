# Refactor audit, November 2026

Plan and evidence for the dependency upgrade and the slimming refactor.
Part 1 is the approved plan (Spanish, as agreed with the maintainer). Part 2
is the raw audit log: first-pass findings by 11 review agents, then the
adversarial verification by 25 agents (`### V-…` sections), with `file:line`
evidence. Where a verification section contradicts a first-pass note, the
verification wins. Delete this file when the refactor is done.

---

# Part 1 — Plan


## Contexto

Cthulhu Git es un cliente Git de escritorio en Rust (edition 2024, toolchain 1.98.1, egui/eframe 0.36). Se escribió con Grok a base de auditorías adversariales sucesivas. Antes de añadir muchas funciones se quiere:

1. Subir dependencias, toolchain y GitHub Actions.
2. Instalar herramientas agénticas para Rust.
3. Con su ayuda, refactorizar y adelgazar el código y los tests con prácticas documentadas, sin parches y sin romper el comportamiento.

**Cómo se hizo la auditoría**
- **Primera pasada:** 11 subagentes revisaron todo el repo.
- **Verificación adversarial:** 25 subagentes intentaron refutar cada una de las ~200 afirmaciones contra el código, el código fuente de las dependencias en sus versiones bloqueadas y la documentación oficial. Context7 agotó su cuota mensual en todos los agentes; las APIs se verificaron en el código fuente (`~/.cargo/registry` y egui 0.37.0 en GitHub).
- **Resultado:** cerca de la mitad de las afirmaciones se confirmaron, unas 35 % solo en parte y alrededor de 15 % se refutaron. El plan ya incluye todas las correcciones.
- Registro completo con evidencia `archivo:línea` en la Parte 2 de este documento.

**Punto de partida medido**

| Métrica | Hoy | Objetivo estimado |
|---|---|---|
| Líneas de producción | ~14 000 | ~11 500 (−18 %) |
| Líneas de tests | ~26 000 | ~12 000 |
| Número de tests | ~575 | ~300 |
| `cargo test` | ~90 s (60 s solo en `adversarial_git_security.rs`) | < 15 s |

- `main` está en rojo por un test desactualizado: la tarjeta queda fuera de la vista a 520×400 desde el logo nuevo. No es una regresión del producto.

**Decisiones del usuario (definitivas)**

| Tema | Decisión |
|---|---|
| Terminal | Se **conserva** el kill del árbol de procesos y el join en un hilo aparte. Solo se quita la duplicación. |
| Historial | Se **conserva** el layout propio, simplificado a funciones puras. Sin `TableBuilder`. |
| Iconos de la UI y logo dentro de la app | SVG renderizado en runtime (`egui_extras`, feature `svg`), nítido a cualquier tamaño y DPI. |
| Icono de la ventana | Sigue siendo el PNG de 256 px que genera `build.rs`. |
| `TERM` | Fijo a `xterm-256color`. |
| Preview del terminal en Settings | Con shell real; solo se separa `show` / `show_preview`. |
| Animación de la rama (glow/cresta) | Se conserva y se mejora (Fase 6). |
| `POSSIBLE_LATER` (roadmap en GitHub Auth) | Se mantiene. |
| LSP | Plugin oficial `rust-analyzer-lsp`; Serena desactivado en este repo. |

## Forma de trabajo

- Una rama por fase y un commit por paso lógico.
- Los bugs van en un commit propio, cada uno con su test de regresión.
- Al cerrar cada fase: verde local, `/code-review` del diff, y se pregunta antes de hacer push y abrir el PR.
- En las fases 3 y 5 se usan subagentes en paralelo, cada uno en su propio worktree (`isolation: "worktree"`) y con archivos disjuntos. Yo integro y reviso.
- Regla de tests: no se debilita ninguna aserción de seguridad. Un test solo se borra si la verificación confirmó que está duplicado, es tautológico o prueba git, std o serde. Las fusiones conservan cada caso como fila.

---

## Fase 0 — Base verde, experimentos y herramientas

1. **Test rojo:** que haga clic con `click_accesskit()` o use una ventana más alta. Es el único test que recorre `open()` → worker → `poll_opening` → `finish_open(Err)` desde una tarjeta, así que se conserva.
2. **Experimentos que deciden el diseño** (se corren antes de tocar nada):
   - **Un commit con NUL en el subject a través de `branch_graph`.** `parse_graph_log` rechaza el log entero si encuentra un registro malo. Si falla, es un bug de producción y se arregla en la Fase 2.
   - **Precedencia de `PROPTEST_CASES` frente a `cases: 64` explícito.** Dos verificadores se contradicen; se resuelve con `PROPTEST_CASES=1 cargo test` y se cuentan los casos.
   - **`pull.rebase=preserve` en el git local:** confirmar si es fatal desde git 2.34.
3. **Configuración de Claude Code** (vía la skill `update-config`):
   - `CLAUDE.md` de menos de 100 líneas: comandos, convenciones (sin `unsafe`, sin `unwrap`/`expect` fuera de tests, `#[expect(lint, reason)]`, `thiserror` por módulo, no añadir dependencias sin preguntar, preferir borrar o fusionar) y política de tests (añadir una fila a una tabla existente; un test nuevo debe reproducir un bug o matar un mutante; un refactor no puede crecer en líneas netas de test).
   - `.claude/rules/testing.md` con `paths: ["tests/**", "src/**/*_tests.rs"]`.
   - Symlinks relativos `.claude/skills/{rust-best-practices,bash-defensive-patterns}` → `.agents/skills/…`. Claude Code no lee `.agents/`.
   - Skill `.claude/skills/slim-tests/SKILL.md` con `disable-model-invocation: true`, que contiene la receta de cobertura + mutantes.
   - Hooks en `.claude/settings.json` (esquema verificado):
     - `PostToolUse`, matcher `Edit|Write`, `"if": "Edit(*.rs)"` → `cd "$CLAUDE_PROJECT_DIR" && cargo fmt --all`;
     - `Stop` → `.claude/hooks/clippy-gate.sh`. Con `jq`, sale si `stop_hook_active` ya es true; corre clippy solo si cambiaron archivos `.rs` y, si falla, sale con 2 y las últimas 80 líneas en stderr.
   - `.claude/settings.local.json`: `{"deniedMcpServers":[{"serverName":"serena"}]}`.
4. **Herramientas** (`cargo install --locked`): `cargo-nextest` 0.9.152, `cargo-llvm-cov` 0.9.2 (+ `llvm-tools-preview`), `cargo-mutants` 27.1.0, `cargo-deny` 0.22.1, `cargo-shear` 1.14.1.
   - Configs: `.config/nextest.toml` (perfiles default/ci/mutants), `.cargo/mutants.toml` y `deny.toml` (lista de licencias verificada contra los 527 crates).
   - En `.cargo/mutants.toml`, `profile` es el perfil de **Cargo**. El perfil de nextest se elige con `NEXTEST_PROFILE`.
   - nextest queda bloqueado hasta resolver la carrera ETXTBSY de la Fase 3.
5. **Líneas base** (en el scratchpad): `cargo llvm-cov` de producción y `cargo mutants` de `git/{log,graph,exec,sync}`, `github/{http,client,resolver}` y `settings`.

## Fase 1 — Dependencias, toolchain y CI (sin egui)

Un commit por grupo; todas las versiones y SHAs están verificados.

1. **CI**
   - `taiki-e/install-action` v2.90.11 `073e1fd0…`. Hay que subirlo junto con zizmor 1.33.1; si no, zizmor se instala con binstall sin verificar el checksum.
   - `upload-artifact` v7.0.2 `cf430e03…`.
   - `download-artifact` v8.0.2 `9000827c…` (ahora un digest que no coincide es error).
   - `action-gh-release` v3.0.4, commit `8a0cd651…` (Node 24).
   - cargo-generate-rpm 0.21.1.
   - Fijar los runners `windows-2025` y `macos-26` también en `build.yml`.
   - Correr zizmor una vez: la 1.33 trae la auditoría `dangerous-checkout`.
2. **`cargo update`** y subir los mínimos de `Cargo.toml`. Con ureq 3.5.2 el límite se aplica después de descomprimir.
3. **resvg 0.50 + ico 0.6** (sin cambios de código) y `RESVG_VERSION` con los dos hashes del CLI. Comparar los iconos renderizados: cambian los filtros.
4. **rfd 0.18** (sin cambios de código; elimina el duplicado de pollster 0.4).
5. **Rust 1.100.0** y `rust-version = "1.100"`.

## Fase 2 — Bugs verificados (un commit y un test de regresión cada uno)

**Seguridad y robustez**
1. **PATH vacío en el padre:** `extend_path_for_gui` construye un PATH que empieza con una entrada vacía, es decir el cwd, que es la raíz del repo. Un git plantado se ejecutaría (`exec.rs:328`).
2. **Windows, mayúsculas en `GIT_*`:** el prefijo `GIT_*` se compara distinguiendo mayúsculas, así que `Git_Dir` sobrevive y Git for Windows lo lee. Afecta a `exec.rs:343` y a `vars_to_unset` del terminal.
3. **Windows, ruta del shell sin comillas:** el programa del shell va sin comillas en el cmdline de ConPTY, así que con `C:\Program Files\…` se intenta antes `C:\Program.exe`.
4. **`parse_name_and_email` cuadrático:** O(n²) con `"<"*N + ">b>"` en trailers de un repo ajeno; la deduplicación de co-autores también es cuadrática.
5. **`core.fsmonitor=false`:** en git 2.16–2.35 ejecuta un comando llamado `false` en cada lectura del índice. Cambiar a `core.fsmonitor=` (vacío) y actualizar los 3 tests que fijan el valor.
6. **`url_without_credentials`:** devuelve la URL cruda si no la puede parsear, y puede mostrar credenciales o la query en el banner de error. Sustituir por un mensaje fijo.

**Funcionales**
7. **`parse_file_diff`:** toma `+price 160000` por un submódulo. Solo debe mirar la cabecera, antes del primer `@@`.
8. **Upstream con `/` en el nombre del remoto:** pasar a un `Upstream { remote, branch }` tipado.
9. **Foco del terminal:** sin `set_focus_lock_filter`, Escape nunca llega a vim, Tab salta a otro widget y las flechas mueven el foco.
10. **OSC 10/11/12/4 responden negro:** deben responder con la paleta.
11. **F1–F4 con modificadores:** deben enviar `CSI 1;mP/Q/R/S`.
12. **`env::vars()` en el terminal:** hace panic con variables que no son UTF-8 y el shell no arranca. Pasar a `vars_os()`.
13. **ureq:** un cuerpo de 2 MiB o más llega como `Transport` en lugar de `TooLarge`. Mapear `BodyExceedsLimit`.
14. **Botones de icono y checkbox de rama sin nombre accesible:** `response.accessible_name(..)`. Lo exige kittest 0.37.
15. **`hover.rs:272`:** `.monospace().small()` dibuja el hash con fuente proporcional.
16. **Textos contradictorios en la UI:**
    - Token: "Still waiting on the keychain…" se queda para siempre (también con errores `Locked` ya terminados), mientras el estado dice "it is not saved".
    - Settings: "using the defaults until a setting changes" contradice el código, que nunca guarda tras una carga fallida.
17. **`CTHULHU_KEYRING=sample` activo en release:** dice "saved in the system keychain" pero el token solo vive en RAM. Desaparece con la inyección de la Fase 3.
18. **Licencias que no se empaquetan:** las de las fuentes por defecto de egui (Hack, Noto Emoji, emoji-icon-font; textos en `epaint_default_fonts/fonts/`) y `assets/icons/LICENSE` (Lucide ISC + Feather MIT). Añadirlas a deb, rpm, tar, AppImage, zip y dmg.

Refutados y descartados: el carril 0 del grafo (es intencional) y el token leído en cada frame (no es un bug).

## Fase 3 — Adelgazar los tests con los puntos de inyección que los abaratan

**Primero, los puntos de inyección en producción**
- **`CthulhuApp::new(ctx, Startup { command_line, settings_path, avatars, git })`.** Elimina `cfg!(test)` en `mod.rs:194` y la mayor parte de los re-exec de la UI, y hace que los 7 pares que hoy solo corren en Unix corran también en Windows.
  - Los tests aíslan el gitconfig del usuario con el entorno que se pasa a git.
  - El re-exec se mantiene solo donde siga siendo necesario: entorno heredado hostil y descubrimiento de git por PATH.
- **`#[derive(Debug, PartialEq)]` en `Action` y `apply_action(&mut self, ctx, action)`.** Browse se queda en `ui()` porque necesita `frame`. Borra las 3 copias de `action_name`/`describe_action`/`action_label`.
- **`exec.rs`:**
  - Separar la construcción del `Command` (argv) de `capture()`.
  - Un test unitario del **prefijo completo** (`--no-optional-locks`, `--no-pager`, `--no-replace-objects`, `-c core.fsmonitor=`, `log.showSignature=false`) con `get_args()`. Va **antes** de borrar `checkout_option_shaped_names…` y `checkout_does_not_run_core_fsmonitor`.
  - `command_with_env(program, parent_env)` probado con `get_envs()`.
- **`branch_graph_within` / `switch_branch_within(timeout)`:** los dos tests de 50–60 s pasan a usar timeouts de 300 ms.
- **`token.rs`:**
  - Worker ordenado con un `Arc<CredentialStore>` inyectado. `keyring-core` pasa a ser dependencia directa; ya está en el lock.
  - Producción instala el store nativo en el hilo del worker, porque `store_status()` hace una llamada D-Bus bloqueante.
  - Los tests usan `keyring_core::mock` y un `CredentialStoreApi` pequeño con compuerta para simular cuelgues. Nada de stores globales: los tests corren en paralelo.
  - Se **conserva** la semántica intencional de revertir un guardado tardío.
  - Se eliminan los 26 `#[cfg(test)]` y `CTHULHU_KEYRING`.
- **`settings.rs`:**
  - `from_json` / `to_json` puros que incluyen el BOM y la normalización.
  - `save_to` **sigue normalizando**: es contrato público probado.
  - `write_atomic`: `NamedTempFile::new_in` → `write_all` → `sync_all` → `into_temp_path()` → `std::fs::rename` → `keep()`. Se usa `fs::rename` para conservar el fallback de Windows. `tempfile` pasa a dependencia normal y `DiskCache` reutiliza la función.
- **Fakes de git:** un solo helper con nombre temporal único + `persist`, para las 4 copias actuales y el staging de `common`. Resuelve ETXTBSY y desbloquea nextest.
- **Código muerto que solo usan tests:**
  - `repo.rs::parse_head` (+ `arb_record_bytes`, `arb_head_bytes`, el helper `status()`).
  - `should_persist`, `checked_when_opened`, `fold_co_authors`, `painted_lane_color`, `CommitFile::diff_path`.
  - `log::history()`/`History`/`parse_log`: después de pasar sus 21 usos a `branch_graph` y `latest_commit`.

**Reorganización**
- `tests/it/main.rs`: un solo crate con módulos por funcionalidad (`support`, `discover`, `open`, `read`, `write`, `hardening`, `process`, `settings`).
  - Ajustar los filtros `--exact` a rutas de módulo (incluido `harness_setsid_sleeper`) y convertir `#![cfg(unix)]` en `#[cfg(unix)] mod`.
  - `isolate()` se integra en el helper que se comparte con la UI.
- `src/test_support.rs` (lib) y `src/ui/test_support.rs`. Se disuelven `author_tests.rs` y `proptest_tests.rs`.
- `docs/ADVERSARIAL-TESTING.md` → `docs/TESTING.md`. Se actualizan las referencias del informe v0.0.8 y de `exec.rs:203`.
- **Proptest**
  - `prop_assert!`.
  - Persistencia de fallos: decidir entre commitear `proptest-regressions/` o mantener `Off`.
  - Los oráculos de co-autores solo se borran **después** de añadir sus casos únicos como filas: `"<a@b.co> trailing"` se rechaza; el señuelo `Co-authored-by` dentro del subject; mayúsculas, tab y CRLF.
- **Tablas:** arrays `const` con `{case:?}` en el mensaje. Sin rstest ni insta.

**Lo que la verificación obligó a conservar** (no se borra):
- **Token y red:**
  - `parse_trims_and_refuses_what_cannot_be_a_token`: única protección contra la inyección de cabeceras con `\n`.
  - La fila de `TooManyRedirects`, más una fila exacta de 4 saltos.
  - Un `assert_eq!` sobre `ALLOWED_HOSTS`; ninguna proptest puede fallar en la comprobación de host.
  - Un test nuevo de que el token del usuario no aparece en los errores.
- **Settings:**
  - `directory_settings_path_is_read_or_write_error`: única cobertura de `SettingsError::Read`. Pasa a unit test y comprueba la limpieza del temporal.
  - `override_var_is_cthulhu_git`: une la constante con los textos fijos.
  - El test del texto de `BadOverride`.
  - El orden de los fallbacks de Linux.
- **SHA-256 y repos raros:**
  - `sha256_checkout_attaches_head` y `sha256_branch_and_history_use_full_oids`: única cobertura de SHA-256. Se fusionan.
  - `dot_git_directory_resolves_to_parent_or_clear_error`: no es tautológico.
  - Como filas: bare + `switch_branch`, árbol sucio que cambia de rama, merge real a través de `branch_graph`.
  - `many_branches`: de `==` a `<= 1+N`.
- **UI:**
  - `recent_cards_keep_the_same_size`, usando `CARD_SIZE` en vez de números fijos.
  - `history_hover_card_names_the_author_and_coauthor`: único test de hover. Se quita su llamada a git real.
  - El test de recent abierto desde una tarjeta.
  - `history_sidebar_toggle_is_saved`: único guardado a través de la UI real.
- **Terminal:**
  - `relative_shell_does_not_execute_a_planted_binary`: única protección de extremo a extremo.
  - La mitad `OnlyCopy` de `osc52`: es el control positivo.
  - Los dos tests de kill de sesión.
- **Git:**
  - El test de post-checkout y smudge, reducido a ~20 líneas.
  - `background_writer_does_not_pin_a_finished_git`.
  - Los `dubious_ownership_*_shell_quoted`, en un bucle.
- **Decodificación:** añadir una fila BMP. La de GIF no prueba la allowlist porque no hay decodificador GIF; en Windows y macOS sí hay de BMP y TIFF. Las filas aceptadas comprueban el tamaño de salida.

**Fusiones con salvaguardas**
- **Config hostil del repo:** 3 drivers de filtro distintos, plantados después de los commits del fixture. Se comprueba y limpia el log después de cada punto de entrada, con un control positivo usando git crudo. Se añaden `diff.external` y `log.showSignature` + `gpg.program`.
- **Entorno hostil:** un centinela por fuente de config con `--get-all`. Se eliminan `GIT_SSH_COMMAND` y `GIT_SSH` en el hijo; hoy el caso ssh es vacío. Dos remotos y controles positivos para shallow, graft y replace.
- **Tabla de pull:** cada fila borra `pull.rebase` y `branch.main.rebase`.
- **UI:**
  - La tabla de la CLI añade una fila de `last_repository` obsoleto y otra de settings corruptas con repo en la CLI.
  - La tabla `poll_graph` cubre:
    - selección distinta;
    - sender desconectado;
    - que no se reinicie con la misma selección;
    - que no se reintente tras un fallo.
- **Helpers de kittest:** se conservan los propios donde hacen falta (padding del hash, columna del grafo, wheel). `output` solo guarda el último frame, así que un `step` con varios eventos oculta copias. `step_dt` se queda en 1/60.

## Fase 4 — egui 0.37 y egui_extras (SVG)

- **Compilación y deprecaciones:**
  - `WidgetType::Other` → `Role::Unknown`; alias `WidgetType` → `egui::Role`.
  - `Id::new` → `Id::unique` (mismo hash).
  - `ui.id()` → `ui.scope_id()`.
- **kittest 0.37:**
  - `check_accessibility` ya cubierto en la Fase 2.
  - `MissingGlyphPolicy::Panic`: instalar fuentes en los harnesses o usar `allow_missing_glyphs`.
  - Contraseña: comprobar que ninguna forma dibujada contiene el token, en lugar de mirar el árbol de accesibilidad.
- **Fuentes:**
  - Activar `monochrome_emoji_fonts` con una dependencia directa de `egui`, porque eframe no reenvía esa feature.
  - `system_fonts` queda activo, lo que gana CJK. Corregir `fonts.rs:3` y `:353`.
  - `set_fonts` en `new()` se aplica en el mismo pass. Los cambios a mitad de frame se siguen aplicando en el siguiente pass, así que la lógica de "pass siguiente" se queda, simplificada.
- **Paneles:** el tamaño guardado se resetea una vez por el nuevo salting de ids. Se menciona en las notas de versión.
- **Iconos SVG:**
  - `egui_extras` con feature `svg` (lleva resvg 0.45 en runtime) e `install_image_loaders`. Se instala desde `icons.rs` o `theme::apply`, protegido con `is_loader_installed`, para que los harnesses que no pasan por `new()` también lo tengan.
  - `include_image!` para los 15 Lucide y para el logo de la app (`assets/icon.svg`).
  - `paint_icon` pasa a `ctx.try_load_texture`.
  - Reemplazar **todos** los `currentColor` (`key-round` también tiene `fill`) y añadir un test de que no queda ninguno.
- **`build.rs`:**
  - Se queda: PNG de 256 px para la ventana, `.ico` + VERSIONINFO con `embed_resource`, y la revisión de git.
  - Se va: los `.rgba`, la lista `UI_ICONS`, `git_executable()` y `windres_quoted` (OUT_DIR ya es directorio de include).
  - La revisión sale de un solo `git log -1 --no-show-signature`, con comprobación hex, con `rerun-if-changed` también sobre `.git/logs/HEAD` y comprobando primero que existe `.git`.
  - Corregir los comentarios de `build.yml:7-9`, `Cargo.toml:41,47-48` y el README.
  - Medir con `cargo bloat` cuánto suma resvg.

## Fase 5 — Refactor de producción por módulo (alcance verificado)

- **`src/git/`**
  - **`capture`:**
    - Un canal y `take().read_to_end` en lugar de 4 hilos.
    - Se conserva: después de que git sale, se espera 200 ms, se mata el árbol, se espera otra vez y se devuelve **Ok**.
    - El hilo principal guarda su propio `Sender` para que el bucle no gire en vacío.
    - Los mensajes llevan etiqueta stdout/stderr.
    - Si se pasa del límite (stdout o stderr) se mata de inmediato.
    - Un error de lectura **no** mata antes de tiempo.
    - El NotADirectory solo se remapea en el fallo del spawn.
  - **`reap.rs`:** se fusionan `group_leader` y `kill_descendants` y sobra `seen`. `kill_tree` (mientras vive) frente a `kill_group` (tras salir). Antes de hacer el módulo `#[cfg(unix)]` hay que poner cfg en `exec.rs:585` y `mod.rs:40`.
  - **Helpers:**
    - `Git::read(cwd, args, what, parse)` para 8 sitios; el de `glance.rs:84` es distinto y se queda.
    - Recorte de fin de línea sin asignar memoria, **conservando las 3 reglas**: LF+CR siempre; CR solo tras LF; CR solo en Windows.
  - **Errores:** `thiserror` muestra `Path` directamente; `Rejected { reason }`; se conserva `TimedOut.limit`, que los tests leen.
  - **`sync`:** fuera el re-export y `name()`; helpers `fetch_args`/`pull_args` probables; un solo predicado de caracteres.
  - **`discover`/`exec`:**
    - `cfg!()` en las 4 tablas por plataforma, con `#[cfg]` en `process_group`, `creation_flags`, `is_executable` y `path_from_bytes`.
    - Sin `LazyLock` para el PATH de GUI.
    - Fuera las variables `GIT_OPTIONAL_LOCKS`/`GIT_PAGER`, que git ya pone; `GIT_NO_LAZY_FETCH` en una línea.
  - **`glance`:** helper privado compartido, una constante de fecha y `is_local_stamp` con plantilla. **No** reutiliza `latest_commit`, porque `%s` es el primer párrafo plegado y no la primera línea.
  - **Descubrir git una vez:** caché de proceso `Mutex<Option<Git>>` que se limpia ante `GitError::Spawn`. Sin retrasar el primer frame.
  - **`graph`:**
    - Ahora: fuera `BottomLane`, las ramas muertas de `occupy` y el `seen`, y `mem::take` (unas −50 líneas).
    - Más tarde, la reescritura a índices (`has_trunk`, `Vec<Option<usize>>` para colores, `first_raw` + `kept`), solo después de añadir tests para: sin rama principal, primer padre fuera de la ventana y segundo dentro, padres duplicados, y puntas fuera de la ventana.
  - **`log`:**
    - Un solo `parse_commit` para detail y hover, conservando las reglas de cada vista.
    - `split_fields` genérico en N (4 llamadas).
    - Un solo `is_oid`: cualquier hex no vacío, también para evitar inyección de opciones.
    - `parse_numstat` con un iterador con estado: los renames rompen el split ingenuo.
    - `DiffLine` se construye una vez.
    - Se mantiene el clamp de `--max-count` (git 2.15 usa `atoi`).
    - `SHORT_OID_LEN` sigue siendo `pub`; `parse_remote_config` pasa a `pub(crate)`.
  - **Opcional:** `Head::Detached { oid }` con comprobación hex. Ahorra un proceso solo con HEAD separado y obliga a tocar muchos tests.
- **`src/github/` + `settings.rs`**
  - **Errores:** `thiserror` con `#[error(transparent)] Http(#[from] HttpError)`, `#[source]` en `SettingsError` y un Display nuevo para `FetchError`.
  - **`read_capped`:** con `take`, misma firma.
  - **zeroize:** `Token(Zeroizing<String>)`, que cumple la promesa de las notas v0.0.6. Se conservan los borrados de las cadenas devueltas por el keychain; fuera los de env y del sample store.
  - **API muerta:** fuera; `Token::new` pasa a `Token::parse().expect()` en los tests. `direct_avatar_url` y `gravatar_url` siguen `pub`.
  - **Tipos:** `UserBody` → `GitHubUser`; `FetchError` con 2 variantes.
  - **Comentario en `http.rs`:** ureq-proto registra las cabeceras crudas, incluido el token, a nivel TRACE. Nunca instalar un logger sin limitar `ureq` y `ureq_proto`.
  - **settings:**
    - Fuera los defaults de serde por campo (el default del contenedor se queda) y el comentario erróneo.
    - El campo pasa a `branches_sidebar_hidden` con `#[serde(rename)]`.
    - `floor_char_boundary`; `normalize_recent` con `HashSet`.
    - Un helper compartido para `ProjectDirs` y uno para `now_unix` (u64).
  - **Se conserva:** el trait `Transport`, el bucle de redirects y `sha2`. **No** se cambia a `http::Response`.
- **UI shell**
  - **Trabajos asíncronos:**
    - `Task<T>` (~35 líneas, con `ready`/`pending` para tests) en los 8 sitios de un solo resultado. No en el escaneo de glance, el pool de avatares ni el terminal.
    - El error se convierte a texto una sola vez, al hacer poll.
    - Los trabajos pertenecen a `OpenedRepo` y `HomeScan`, así que los guards de `view_id` sobran por construcción. Se borran el contador de generación y el filtro `glance_paths`, que son código muerto.
  - **`ui/settings.rs`** (subconjunto verificado, unas −150 líneas):
    - `body`/`help`/`link`/`field_font`.
    - `const PAGES`.
    - Un `selectable_row(margin)` compartido con `changes.rs`.
    - Fuera el `item_spacing` repetido.
    - **Sin** `interact_size` ni `corner_radius`/cursor globales: cambiarían checkboxes, spinners y filas.
  - **Tema:** fuera las 28 llamadas `.color(text)` que no hacen nada; 40 pasan a `.weak()`, quitando `.strong()` en `home.rs:194` y `history.rs:523`.
  - **`repo_view`:**
    - `bottom_bar()`, `SyncKind::progress_label()`.
    - `Action::SetPanels(Panels)` con valores absolutos.
    - `bar_frame` y el grupo de navegación compartidos.
    - `shown_oid()` como función que se recalcula.
    - El commit más reciente y el seleccionado siguen por caminos separados, compartiendo piezas.
  - **Estado:** `save_path: Option<PathBuf>` con una función probada. `SettingsScreen` solo une la preview con la bandera de abierto; la página elegida se mantiene en la sesión.
  - **Varios:** `Facts`, rellenos al final, `Default` derivado, `Theme.name` (+ docs), `process::exit`.
  - **Comentarios:** fuera la narración y se corrigen los 4 erróneos (`mod.rs:173`, `872`, `124`/`684`, `160`).
- **UI history / terminal / hover / changes / avatars**
  - **history:**
    - Funciones puras `layout_columns`, `drag_columns`, `reset_column`, `empty_history`, `toggle_lit`, con un proptest de invariantes.
    - Menos visibilidad (`pub(super)` innecesarios).
    - Seleccionado y encendido pasan a `OpenedRepo`, con las reglas actuales: retain mientras carga, encendido olvidado sin filas, repintado cuando llega el grafo.
  - **`AsyncCache<K, V>`:** sustituye `HoverCache` y `DiffCache` con LRU al leer. **Corrige el bug** de que nunca se libera nada; test a través de `start`/`poll`. `DiffCache` deja de crear la clave cada frame. La tarjeta de autor sigue siendo propia: el `Tooltip` de egui cambia el comportamiento.
  - **terminal:**
    - Se conservan el kill y el hilo aparte. `should_signal` (con comprobación de vida y usado en macOS) se queda en el bin.
    - Funciones `#[cfg(unix)]`/`#[cfg(windows)]` en lugar de `HostOs`.
    - Guardia de resize como función pura y comprobación barata primero.
    - `resolve_color` (Cursor y BrightForeground → texto).
    - Teclado: rama muerta de Ctrl+C, `arrow` fusionado, `letter` por rango, Home/End en modo app-cursor; tabla de `special_key(.., app_cursor)`.
    - `point_to_viewport`/`viewport_to_point` de alacritty.
    - `TERM` fijo (se mantiene `COLORTERM`). Separar `show`/`show_preview`.
    - **No** cambian el divisor ni la selección: cambio de comportamiento con poco beneficio.
  - **changes:** `widget_info` en la respuesta de la fila en vez del Label invisible; un solo `match DiffKind`; la cabecera se construye una vez.
  - **Ajuste de texto:** `truncate_at_width` y un helper con `break_anywhere: true` para las 4 copias (incluida `place_sidebar_text`).
  - **avatars:** `generation: u64`, `write_atomic` de la lib, `CornerRadius::from`. Opcional: orientación EXIF.

## Fase 6 — La animación de la rama (PR dedicado)

1. **Red de seguridad:**
   - Extraer una función pura que construye la malla del glow.
   - Test golden de vértices e índices a un tiempo fijo.
   - Ese test sustituye los ~150 líneas de helpers que hoy leen los vértices de la malla.
2. **Refactor idéntico bit a bit:**
   - `ribbon()` muestrea una vez y sombrea cada parada.
   - `rings(center, base, steps, &[(scale, color)])`.
   - Se conservan la salida temprana `crosses`, los guards, el orden de vértices y las dos mallas a dos profundidades.
3. **Mejoras.** Cada una se enseña con capturas antes de quedarse:
   - **Repintado solo si una fila pintada contiene el color encendido** (después de `show_rows`) y la ventana tiene foco. Hoy repinta cada 16 ms aunque esté fuera de pantalla.
   - **Fundido** al encender y apagar con `animate_bool_with_time`: id estable llamado en cada frame, se guarda el último color para el fundido de salida, y A→B con dos animaciones.
   - **Juntas a inglete** en los cambios de carril, para quitar la muesca sin glow en las diagonales.
   - **Sin doble brillo** en los anillos de los puntos y en las fusiones.
   - **A evaluar:** luz aditiva en la cresta (`from_rgb_additive`); satura hacia blanco y hay que ajustar los tests que exigen alfa > 0.
   - **Prototipo opcional:** transiciones Bézier entre carriles.
   - Descartado: una sola malla continua. Las costuras entre filas no existen y rompería el orden de profundidad con la selección.

## Fase 7 — Lints, CI y empaquetado

- **Lints:** `pedantic` como warn con `allow` en casts, documentación y `must_use`; restricciones `unwrap_used`, `expect_used`, `dbg_macro`, `todo`, `allow_attributes_without_reason`.
- **`clippy.toml`:** `allow-*-in-tests` (cubre `tests/` desde 1.100), `avoid-breaking-exported-api = false`, `too-many-lines-threshold = 100`.
- **Guardia estática:** `disallowed-methods` sobre `std::process::Command::new`, con allow por módulo en tests y en los ~7 sitios de producción.
- **Cargo 1.100:** `[lints.cargo] unused_dependencies`, y las simplificaciones `trim_prefix`/`trim_suffix`, `bool::toggle`, `from_utf8_lossy_owned`.
- **CI:**
  - `cargo nextest run --profile ci` en Linux.
  - `cargo shear`.
  - `cargo deny check licenses bans sources`.
  - Workflow semanal con `advisories`.
- **Opcional:** `.cargo/config.toml` con `registry.global-min-publish-age`.

## Fase 8 — Documentación y versión

- README, `SETTINGS.md` (campo renombrado), `FONTS.md`, `THEMES.md` y `docs/TESTING.md`.
- Notas de la v0.0.9, que incluyen los bugs corregidos y el reset único del tamaño de los paneles.

---

## Verificación

1. **En cada commit:** `cargo fmt --check`, `cargo clippy --locked --all-targets -- -D warnings`, `cargo nextest run` (o `cargo test --locked` hasta que se arregle ETXTBSY) y `cargo check --target x86_64-pc-windows-gnu` cuando se toque código con `cfg`.
2. **Cobertura:** `cargo llvm-cov nextest` después de cada área de la Fase 3. Las líneas de producción cubiertas no pueden bajar, salvo las borradas.
3. **Mutación:**
   - `NEXTEST_PROFILE=mutants cargo mutants` sobre los mismos archivos que la línea base.
   - `caught.txt` no puede encoger salvo por código borrado (`comm -23 base cand`).
   - cargo-mutants no dice qué test mató cada mutante, así que se compara por grupos.
   - Los tests de amenazas solo se fusionan, nunca se borran.
4. **Invariantes:** al cerrar la Fase 3 se revisa la tabla invariante → test superviviente de `findings.md`.
5. **Visual:**
   - Capturas del binario release bajo Xvfb (`xvfb-run`, `xdotool`, `import`) antes y después de las Fases 4 y 5 (Settings, iconos) y de la animación, en instantes fijos.
   - Las leo como imagen y te las enseño. Si faltan paquetes, te pido que los instales.
6. **Manual (`cargo run`):** repo y recientes; fetch, pull y cambio de rama; terminal (vim con Escape, Tab y flechas); Settings (fuentes, token); avatares; diálogo de carpeta; emoji y CJK.
7. **CI del PR:** matriz Linux, macOS y Windows, más `build.yml` (deb, rpm, AppImage, Windows y macOS).

---

## Progreso

- [x] Fase 0.1: test rojo arreglado (`click_accesskit`).
- [x] Fase 0.2: experimentos. NUL en subject: OK, no es bug. `PROPTEST_CASES` pisa `cases: 64`, no hace falta `[env]`. `preserve` con `--no-rebase`: se rechaza por divergencia, fila válida.
- [x] Fase 0.4: herramientas instaladas (nextest, llvm-cov, mutants, deny, shear).


---

# Part 2 — Audit log


## USER DECISIONS (round 1)
- Terminal: delegate to alacritty (BASED ON WRONG INFO -> re-ask; see V-arch #4)
- History: migrate to TableBuilder (BASED ON OVERSTATED INFO -> re-ask; see V-egui #5)
- Icons SVG runtime: yes (incl. app icon/logo at any size). TERM fixed: yes. Static preview: NO (keep live). Static highlight: NO — keep & IMPROVE animation ("mucho ojo a la rama y al glow").
- LSP: official plugin, disable Serena in repo.
- User asked: verify ALL ~150 findings adversarially (25 agents).

## VERIFICATION ROUND (adversarial)
### V-egui (0.37 APIs)
1 set_fonts PARTIAL: applies same pass only if no text laid out yet this pass; else next pass -> keep 2-pass handling for mid-frame changes; set_fonts in new() is immediate.
2 Id::unique/scope_id/WidgetType->Role CONFIRMED; WidgetType::Other -> Role::Unknown/Splitter.
3 kittest: check_accessibility default true; MissingGlyphPolicy::Panic default; allow_missing_glyphs(). `accessible_name` is on RESPONSE (response.rs:1087), not Button builder. Harness has no font provider -> glyphs from emoji fonts (e.g. "✓" network_tests.rs:450) panic.
4 eframe defaults default_fonts+system_fonts; egui default=[]; monochrome_emoji_fonts not forwarded by eframe -> add direct egui dep feature (at least dev) ; fonts.rs:313 loses emoji fallbacks; "nothing read from OS" false unless NativeOptions::system_font_fallback=false.
5 TableBuilder REFUTED/partial: resizable/at_least/range/header/rows/set_selected/persistence(serde) OK; BUT double-click divider = auto-size (and apparently never fires due to id mismatch), NOT reset; proportional widths impossible (TableState private, absolute pts; no rescale on window resize); no built-in horizontal scroll (wrap in ScrollArea::horizontal -> vertical scrollbar scrolls away); connector painted over by cell bg (workaround inside Graph closure + set_clip_rect); divider a11y names lost.
6 SVG loaders CONFIRMED (crisp per size, tint, image_tint_follows_text_color). load_svg_bytes_with_size(bytes, SizeHint, &usvg::Options) -> need direct resvg dep matching egui_extras' resvg version (0.45.x); premultiplied -> to_srgba_unmultiplied for IconData. resvg in binary.
7 Panel::bottom inside Ui OK; 0.37 id salted by parent scope -> saved panel sizes reset once.
8 Tooltip PARTIAL: 0.5s delay, hides on scroll/click, only stays if interactive widgets -> KEEP hand-rolled author card.
9 Focus lock bug CONFIRMED: Escape clears focus in begin_pass; Tab moves focus before has_focus check; arrows: 1st press delivered then focus moves. Fix: set_focus_lock_filter every frame while focused.

### V-bugs
1 parse_file_diff submodule false positive REAL (med) — also substring "1600000".
2 graph placeholder NOT A BUG (design: lane 0 belongs to main; test unrelated_histories_do_not_meet).
3 reap after reaped pid REAL but theoretical (low); walk useless after reap; walk still needed pre-reap (timeout path, setsid grandchildren).
4 Windows GIT_ case REAL (low); terminal vars_to_unset same check.
5 parse_name_and_email O(n²) REAL (med) with "<<<…<>>" (not "<<<…<>"); also dedupe at log.rs:116 quadratic in many co-authors. Bound only 8 MiB output.
6 upstream '/' remote REAL (low): can force-write wrong remote's ref.
7 OSC 10/11/12/4 answers black REAL (low).
8 F1-F4 modifiers REAL (low): should be CSI 1;mP/Q/R/S.
9 env::vars panic REAL (low-med): shell never starts if any non-UTF-8 env var.
10 env token every frame NOT A BUG.
11 RED TEST = STALE TEST: card below fold at 520x400 after new logo; Node::click outside visible area. Fix with click_accesskit/bigger window/scroll.

### V-arch
1 keyring-core PARTIAL: API exists; DEFAULT_STORE global RwLock -> parallel tests collide -> inject Arc<CredentialStore> into worker; needs direct keyring-core dep; keyring::Entry::new LazyLock may silently install OS store; store_status() blocking D-Bus -> worker thread; mock can't hang -> small test-only CredentialStoreApi with a gate; set_error one-shot consumed by save's pre-read; must still decide late-save semantics.
2 tempfile PARTIAL: no fsync (keep sync_all); loses std rename's Windows ACCESS_DENIED fallback -> use into_temp_path()+std::fs::rename+keep(); 0600; tests asserting <pid>.tmp naming to rewrite (adversarial_settings.rs:313-351, 419-436, avatars.rs:818-868); crash leftovers random names. tempfile is dev-dep only now.
3 ureq CONFIRMED + EXISTING BUG: 3.4.2 LimitReader errors at budget 0 even at EOF -> body >= 2 MiB (incl exactly MAX_BODY) -> Transport not TooLarge; avatar too big -> Failed not Missing. Fix: map ureq::Error::BodyExceedsLimit (downcast io::Error) -> TooLarge; on 3.5.2 drop read_capped.
4 alacritty Pty REFUTED: Drop = SIGHUP + BLOCKING wait(); may run on UI thread if loop ended; shell ignoring SIGHUP blocks forever (master fd still open) -> leaks; setsid/nohup jobs survive; Windows ClosePseudoConsole may block, needs taskkill. => KEEP side-thread join + session kill (dedupe with reap.rs).
5 Stale guards PARTIAL: graph_generation (mod.rs:935) and glance_paths (:326) dead; view_id guards REACHABLE (fetch on A then open B: sync_job never cleared; graph load + unborn B). => moving jobs into OpenedRepo ownership makes them unnecessary; do it as ownership move, not deletion.
6 faces_ready CONFIRMED app-only; 2 harnesses draw Appearance page without sync -> install fonts in harness first.
7 re-exec PARTIAL: also needed for Git discovery env (PATH, CTHULHU_GIT; 5 cases), HOME for user gitconfig isolation (7 real-git cases), process isolation of statics/threads. => Startup must inject Git (discovered) + git env (HOME/config isolation).

### V-colors/avatars/changes
1 colors CONFIRMED; keep index<COUNT check in color_rgb (usize from ColorRequest, panics OOR). vte: 0-15, Fg 256, Bg 257, Cursor 258, Dim 259-266, BrightFg 267, DimFg 268. resolve_color must map Cursor & BrightForeground to palette.text (else green/yellow). Dim arms unreachable; renderer ignores Flags::DIM (SGR 2 not rendered — possible improvement).
2 splitter PARTIAL: fraction is egui temp (not persisted); Panel::bottom not resizable by default, persists via eframe storage keyed by view_id ("repo-view", n) -> Nth repo in session; must recompute max_size per frame; lose small-window rule; panel order reversal; separator style changes. => lower value; optional.
3 avatars CONFIRMED: generation -> u64; disabled() via cfg!(test) needs #[cfg(test)] blocks or injection; DiskCache::write dup but settings in lib + save_lock -> shared helper in lib; now_unix u64 vs i64 + inline copies; clamp no-op -> CornerRadius::from(radius).
4 changes PARTIAL: ui.put centers (centered_and_justified) + a11y text includes line numbers -> breaks history_tests.rs:2249 -> instead keep painting + response.widget_info(WidgetInfo::labeled(Label, &line.text)). 4 DiffKind matches -> one tuple. heading built twice.
5 wrapping PARTIAL: elided == truncate_at_width (keep .max(0)); wrap_at_width has break_anywhere:false -> use TextWrapping{break_anywhere:true, ..wrap_at_width(w)}; 4th copy place_sidebar_text repo_view.rs:885-897.
### V-deps/CI
1 Actions SHAs CONFIRMED (checkout latest; install-action v2.90.11 073e1fd0… published today; upload v7.0.2 cf430e03…; download v8.0.2 9000827c… digest error default; gh-release v3.0.4 commit 8a0cd651… (annotated tag 28ab305d…), node24).
2 tools CONFIRMED; bump install-action WITH zizmor (else binstall fallback w/o checksum); cargo-generate-rpm always binstall (not checksum pinned); zizmor 1.33 new dangerous-checkout audit.
3 resvg CLI 0.50.0 hashes CONFIRMED (downloaded).
4 runners CONFIRMED; windows-2025 ships VS 2027; D: ReFS from 2026-11-30; build.yml:326 macos-latest, :216 windows-latest.
5 crates CONFIRMED: rfd 0.18 no code change, drops pollster 0.4; resvg 0.50 API unchanged (pixel caveat); ico 0.6 no change, drops png 0.17; ureq 3.5.2 ok; proptest 1.12 MSRV 1.88 ok; patch bumps ok. egui_extras 0.37 svg -> resvg ^0.45.1 (+tiny-skia 0.11) => TWO resvg families if build.rs stays on 0.50.
6 Rust 1.100.0 (2026-11-12) CONFIRMED: trim_prefix/suffix (str,[T],Path), bool::toggle, DropGuard (1.100), from_utf8_lossy_owned (1.99), [lints.cargo] unused_dependencies; min publish age is .cargo/config.toml (registry.global-min-publish-age).
7 all deps used CONFIRMED.

### V-tooling
1 skills CONFIRMED (.claude/skills, symlinks ok, disable-model-invocation).
2 hooks CONFIRMED: `if: "Edit(*.rs)"` (matches Write too), command+args, timeout secs, Stop exit 2, stop_hook_active, $CLAUDE_PROJECT_DIR; JSON + clippy-gate.sh (jq) recorded by agent. 8-in-a-row stop cap.
3 deniedMcpServers works (hard block, any file); softer: /mcp toggle -> disabledMcpServers in ~/.claude.json project entry. disabledMcpjsonServers wrong.
4 plugin enabled + serena user-scope CONFIRMED.
5 versions CONFIRMED (nextest 0.9.152, llvm-cov 0.9.2, mutants 27.1.0, deny 0.22.1, shear 1.14.1, insta 1.49.0, rstest 0.29.0, cargo-edit 0.13.14). None installed.
6 mutants `profile` = CARGO profile (not nextest) -> select nextest profile via --cargo-arg/ env NEXTEST_PROFILE; deny_unknown_fields; no per-test kill attribution (only caught/missed lists).
7 clippy: redundant_clone nursery; allow-unwrap-in-tests covers tests/ helpers only from Rust 1.100 (clippy #16786).
8 deny license list CONFIRMED complete.
9 egui default fonts licences NOT shipped (Hack, emoji-icon-font MIT, Noto Emoji OFL) -> texts in epaint_default_fonts-0.36.2/fonts/.
10 [env] doesn't override existing env. CONFLICT with V-git-unit-tests (#6 says env overrides explicit cases via contextualize_config) -> verify empirically.

### V-git head/sync/discover
1 Head::Detached{oid} PARTIAL: different validations (hex check) -> add hex check in detached_head; saves 1 call only while detached; many test updates; head_commit_oid is public API used by tests.
2 sync PARTIAL: SwitchRequest/SwitchKind/switch_request USED by switch_branch; only re-export + name() unused. Keep testable fetch_args/pull_args (pull test only lock on --no-rebase).
3 cfg!() PARTIAL: 4 allows; keep #[cfg] on process_group, creation_flags, is_executable, path_from_bytes; add unix/else branch. Keep extend_path_for_gui pub; LazyLock freezes Homebrew detection (behavior change) -> don't LazyLock.
4 unquote OK (only matters on Unix; Windows split_paths strips quotes).
5 glance reuse REFUTED behavior: %s = folded first paragraph; validation differs; reading %B per recent repo heavy -> share private helper + date const + template loop for is_local_stamp.
6 drop GIT_OPTIONAL_LOCKS/GIT_PAGER env CONFIRMED (git sets them itself for children).
7 core.fsmonitor: empty `core.fsmonitor=` correct for all versions; on 2.16-2.35 "false" RUNS a command named false on every index read (switch/pull) -> real bug-ish; Windows spawn error. Fix as separate behavior commit + update 3 test sites. fsmonitor arrived 2.16 (comments say 2.15).
8 GIT_NO_LAZY_FETCH collapse CONFIRMED.
9 discover once PARTIAL: risks — first frame delay up to 250ms/candidate; retry after Spawn error; upgraded git mid-session stale; caches lack app access -> process-wide Mutex<Option<Git>> cleared on Spawn error.
NEW BUG: no PATH in parent -> extend_path_for_gui builds PATH starting with empty entry = cwd = repo root (planted git risk!) (exec.rs:328).

### V-repo_states/repo tests
- Location errors (pull tests, normal_repository in security; many_branches in readonly; nfd in macos). history() also used at security.rs:740. Keep Commit type (ui fixtures) — or switch fixtures.
- sha256_checkout_attaches_head REFUTED (only SHA-256 coverage of latest_commit/branches) -> fold into repointed SHA-256 read test. sha256_branch_and_history_use_full_oids unique -> repoint.
- dot_git_directory REFUTED (meaningful; may fail on git<2.25 — verify on old git).
- checkout_keeps_uncommitted_edit, bare_checkout_fails_cleanly (bare repo + switch_branch no guard), merge_commit (no real-git merge via branch_graph) -> keep as rows/repoint.
- many_branches: change == to <= 1+N (only spawn bound).
- branch_graph +1/truncate code untested for "exactly N not truncated"; usize::MAX clamp in branch_graph untested -> repoint.
- Pull table sound only if each row unsets pull.rebase AND branch.main.rebase (preserve fatal on git>=2.34), checks markers/dirs, names row. On git>=2.34 --ff-only wins anyway.
- Switch rows keep row-specific checks; remote-only switch error type varies by git version (is_err + no ref); reject table rows need expected error/path + cfg gates; bare test tighten to Ok.
- 21 tests call history(); all expressible via branches()/graph_tips/latest_commit/graph rows. MUST RUN: NUL in %s through parse_graph_log — if it fails it's a PRODUCTION BUG (whole graph rejected).
- Unique keep list recorded (unborn_repository_lists_its_branch, commit counts, co-author trailers, commit_hover x2, remotes order, mailmap, graph leaves main, detached on graph, inherited_git_dir + child, glance x3, detached_head_opens_when_history_walk_fails).

### V-UI adversarial tests
1 7 of 11 children NOT cfg(unix) -> compile as ignored no-ops on Windows; `--ignored` alone = green with zero coverage. In-process app with injected settings path drops re-exec for 7 non-graph pairs.
2 recent_cards_keep_the_same_size REFUTED (only test of CARD_SIZE/wrap; reference CARD_SIZE instead of 300x168). a_narrow_history… weaker than thought (passes w/o table) -> delete + fix comments history_tests.rs:29,1343; add 520 to width loop. adversarial_missing_recent… REFUTED as deletion: only test of open() -> worker -> poll_opening -> finish_open(Err) from card click -> keep as row, remove if-guard (and fix click: card offscreen).
3 tables: cli add rows (stale last_repository at startup -> Home+forgotten+saved; corrupt settings + cli repo -> file untouched). Browse: Action::Open has NO busy guard (cards hover-only sense) -> untested; busy computed in mod.rs untested. Hostile names: carry tooltip order/no overlap/spare width checks; sidebar open/closed column. Branch menu: click_accesskit leaves menu open -> drop "menu closed" asserts.
4 corrupt-settings test never saves CONFIRMED; chmod as root -> test fails red (not false green). Home/toggles need apply_action extraction. poll_graph rows: + selection mismatch, disconnected sender, sync_graph no restart while same selection in flight, no retry after failure, history_loading(). Hand-sequenced unit tests fine; hold sender, step real ui(), assert send().is_err().
5 isolate() needed (strips GIT_*, sys config, hooks, signing) -> inline it.
6 slow: settle_open no-op in most; module doc stale.
7 kittest: query_all_by_role_and_label(Role::Label, x) exact equivalent; get_by_label panics on multiple matches; debug_fmt_node lacks rects.
- EXISTING BUG/TEXT: settings error "using the defaults until a setting changes" contradicts code (never saves when load failed).
- GAP: running fetch vs new open untested; busy ignores sync_job.
- unmentioned keep: history_sidebar_toggle_is_saved + child (only real-UI save test), load_failure_sets_loaded_ok_false, selection_tests.

### V-other integration tests
1 relative_path_entry… PARTIAL: only test planting a real git reachable by relative entry -> fold into unit test (needs no re-exec). others CONFIRMED.
2 override_var_is_cthulhu_git REFUTED: links constant to hard-coded strings (exec.rs:73,110, README, ui tests) -> move to unit test or assert error starts_with(OVERRIDE_VAR). Others CONFIRMED.
3 directory_settings_path_is_read_or_write_error REFUTED: only test of SettingsError::Read branch (protects D5 "don't persist") -> move to unit test + assert tmp cleanup. concurrent_saves only guard of save_lock (2-4 bursts).
4 macos file removal PARTIAL: missing_workdir -> unit; git_no_lazy_fetch needs env-injection refactor; fsmonitor covered by prefix; nfd -> read module.
5 Cargo book: "may want to consider" (suggestion). Costs of single crate: --exact module paths incl harness_setsid_sleeper SCRIPT, cfg(unix) crate attrs -> mod attrs, #[path] include, area-ownership doc rule.
6 ETXTBSY under nextest CONFIRMED (+ common staging <stem>.tmp shared inode) -> NamedTempFile::new_in + persist for all 4 helpers.
7 lints apply to tests CONFIRMED (cwd re-exec reason is shared cwd, not lint).
8 disallowed-methods works for Command::new; ~27 test sites (module allow) + ~7 prod sites.
9 naming: docs depend on file names (area ownership); update docs/reports/exec.rs:203/ui tests filters.

### V-caches/glow/state
1 PARTIAL + HIDDEN BUG: HoverCache/DiffCache NEVER EVICT in real use (start() inserts Loading bypassing order; insert sees key present) -> unbounded growth (256 cap never applied). Generic cache fixing it enables eviction for first time -> must be LRU-on-get or pin displayed keys; add test through start/poll. Failed never retried (same both). DiffCache builds key strings every frame. Poll sites differ (hover once/frame; diff twice).
2 glow helpers doable with care: sample once then shade stops (u8 double rounding vs float lobe); keep crosses exit; rings with (scale,color) & base radius; keep guards/!mesh.is_empty()/vertex order/two meshes at two depths. ADD golden mesh test at fixed time BEFORE refactor.
3 seams REFUTED at row boundaries (flush, clipped per cell). Real: diagonal rows leave a small unglowed wedge (notch); overlaps where glow disc ring sits over ribbon halos, converging ribbons at merges, crest disc over crest segments. One continuous mesh NOT justified & breaks depth order w/ selection fill. Targeted improvement: mitred joints + fix disc overlap.
4 selection state PARTIAL: keyed by view_id (fresh per open); rules: retain_selected keeps during loading w/ empty rows; lit forgotten when rows empty (asymmetry). Repaint at 1284 needed (graph load lands; right panel drawn first) unless retain moves to graph install.
5 visibility CONFIRMED (+ Column::ALL/title, ColumnLayout pub fields).
6 repaint: scrolled-out CONFIRMED; unfocused CONFIRMED; minimized REFUTED on X11/Win/mac (eframe skips); Wayland uncertain. Fix: request after show_rows only if a painted row contains lit color.
7 additive CONFIRMED (One, OneMinusSrcAlpha both backends): gamma-space add saturates toward white; overlaps brighter; tests history_tests.rs:1615,1648-1653 require alpha>0.
8 animate_bool_with_time CONFIRMED w/ caveats: first call returns target (need stable id called every frame from false); fade-out needs last lit color kept; A->B switch needs two animations.

### V-build.rs/icons
1 git_executable defense useless CONFIRMED (correct paths; build script exe dir sibling of OUT_DIR).
2 PARTIAL: no CI windows-gnu job (manual README step); winresource bad fit (ProductName check build.yml:240, missing windres errors, language) -> KEEP embed_resource; windres_quoted unneeded (OUT_DIR is include dir -> `1 ICON "cthulhu-git.ico"`); single git log: keep rerun-if-changed (+ .git/logs/HEAD), --no-show-signature, hex check, check .git exists.
3 egui hover-grow quirk CONFIRMED; keep workaround (also Button::image caps image at row height).
4 icon lists x4 (name, rgba, test list, UI_ICONS) + README; cache ignores ppp (64px aliasing at 1x, logo blurry >2x); logo PNG decoded twice.
5 packaging independent CONFIRMED; migrate main.rs + icons.rs together; build.rs still needs resvg+ico for .ico; fix comments build.yml:7-9, Cargo.toml:41,47-48, README:285-313.
6 window icon at startup PARTIAL: Windows running-window icon (taskbar/alt-tab) comes from window icon (eframe WM_SETICON) -> needs good 256px; macOS uses icns; Wayland uses app_id. Risks: direct resvg 0.45.1 dep (egui_extras 0.36 pin), premultiplied->unmultiplied, heavy filters (feTurbulence, blurs) on main thread unoptimized in debug, renderer version mismatch vs CLI 0.48/0.50. => ALTERNATIVE: keep build.rs rendering icon-256.png for the WINDOW icon (cheap, identical), runtime SVG only for UI icons + in-app logo.
7 key-round.svg also fill=currentColor -> replace all + test no currentColor; keep @license comments. LICENSING GAP: assets/icons/LICENSE (ISC + MIT Feather) not shipped in any package -> add to deb/rpm/tar/AppImage/zip/dmg.
Other: tests render views without CthulhuApp::new -> install loaders from icons.rs (is_loader_installed guard) or theme::apply; paint_icon needs texture id -> ctx.try_load_texture.

### V-git_security tests
- ORDER: write full argv-prefix unit test FIRST (needs Command built separately from capture()), then delete checkout_option_shaped_names (only full SWITCH_EXEC_PREFIX check; --no-replace-objects otherwise unchecked) and checkout_does_not_run_core_fsmonitor.
- checkout_rejects_dotdot -> replace w/ unit test Git{path:"/nonexistent"} expecting Rejected (not Spawn).
- alias.checkout tests delete CONFIRMED.
- post-checkout/smudge: keep ~20-line version (only test of "switch runs hooks/filters like terminal").
- hostile config merge: 3 distinct filter drivers (process beats clean if same name); plant after fixture commits; textconv needs committed diff attr; check+clear log per entry point; positive control via raw git (not Fixture::git). Aliases/pager/hooksPath/fsmonitor on reads can never fail (only filters real). Add untested: log.showSignature+gpg.program, diff.external. Keep hover_and_file_diff_do_not_run_textconv separate (isolated HOME).
- env merge: per-source sentinels + --get-all; strip trace vars on sanity cmd; remove GIT_SSH_COMMAND/GIT_SSH in child (ssh test vacuous today!); assert real results; two remotes; COUNT slots=4; add positive controls for shallow/graft/replace; exec_path dashed-commands test can never fail (keep remote-helper variant). command_with_env needs refactor first.
- slow tests: add branch_graph_within/switch_branch_within(timeout) or timeout field on Git; losing 60s value OK.
- bombs: cap 8 MiB on stdout+stderr; keep both callers (inspect + branch_graph), 9 MiB; stale comment line 133; optionally add stderr bomb.
- unmentioned uniques: dubious_ownership_*_shell_quoted (one loop), slow_version_probe, background_writer, pull tests -> table (preserve may be rejected by modern git -> check), pull ff control, --upload-pack=evil, include_path default remote, child_opens_repo helper.

### V-history/inline UI tests
1 dead helper CONFIRMED (revives if TableBuilder draws header on empty).
2 Action derive Debug CONFIRMED (3 copies).
3 selection tests -> one scripted test only in specific order; copy tests table POOR FIT (keep separate, maybe share fixture); width test: 3 cases needed (proportional + overflow/cull at 900); halo test needs pure glow-mesh fn extracted (ties into animation refactor!); drag-min tests moot after migration.
4 kittest helpers PARTIAL: drop_at queues PointerGone; output overwritten per frame -> copied_texts only last frame (multi-event step hides copies!); Node::click can't hit padding/graph; no wheel helper; run_ok gives up after 4 steps; step_dt 0.25 default breaks double-click -> keep 1/60. => keep custom input helpers where needed.
5 history inline proptest OK (properties listed) — moot if layout_columns removed.
6 terminal: argv table OK; prompt_span table OK; term_name -> fixed TERM check in shell_env_from; planted-binary test REFUTED as deletion — only E2E guard (alacritty falls back to $SHELL with Command::new in repo cwd) -> KEEP; osc52 OnlyCopy half = positive control -> KEEP (drop config assert_eq); resize test doesn't really cover -> pure decision fn test; kill tests: KEEP (we keep kill behavior); key encoding table needs app_cursor param.
7 fonts: catalog test -> one-line id overlap; ppp test delete; off-disk tautological (keep "resolves in catalog" property); 35-load -> pure definitions() + one real load for PROMPT_ICON nerd glyph.
8 theme test also guards set_theme/set_visuals_of; sync_offer add rows Unborn/missing branch/unfetchable upstream; github_auth keep stored column; main add relative->absolute row.
TableBuilder post-migration test list (11 behaviors) recorded.

### V-UI network tests
2 token test keep: PasswordInput role, History-page leak check, survives page move, assert_no_token_material on URLs/copied text; About nav order.
3 decode table: rows for PNG & JPEG both directions; accepted rows check output size (97->96,180x90->90,1024x16->16, 300x200->96 crop+shrink), PNG crop band. Timing check protects nothing; header-only row keep w/ CRC helper. GIF row doesn't test allowlist (no GIF decoder) -> ADD BMP row (bmp/tiff enabled on non-Linux targets!) to really test allowlist.
4 settings page table must keep per-page font lists & "no action before click with saved fonts" per row; drop line 1088.
5 proptest moves OK (keep FileFailurePersistence Off or regs under src/). relative_time proptest weak.
6 issue_segments markers: move 12 strings as rows to hover.rs (ü#9 multibyte!). relative_time: add boundary rows (1h,1w,30d,365d ±1s) + 3 extremes. about_puts_copyright: only check of "© 2026 Don Linux" text + logo a11y label -> keep text check. history_hover_card REFUTED dup: only test that hovering message doesn't open card & co-author shows before load -> keep (fix real git call). disabled_avatar_service delete OK.
7 avatars merges OK w/ rows (AlwaysTimeout: no files at all; Offline second texture() None); symlink test trims.
8 password a11y: assert on drawn shapes (no text shape contains token) instead of tree for masking.
Unmentioned keep: a_person_keeps_one_color, a_disabled_service_never_queues_work, font_warning_does_not_interpolate_unsanitized_text.

### V-git unit tests
1 parse_head delete CONFIRMED (+ arb_record_bytes, arb_head_bytes, Head import, status() helper, OID const; keep head_display_matches_ui_wording).
2 Ord tests delete OK; error_messages_name_the_override_variable = only test of BadOverride text -> keep (or row); linux_fallbacks order matters -> keep as row.
3 Oracle tests REFUTED: VS Code co-author matching is a product rule; oracle not a copy; deleting lines 26-135 breaks 4 tests; add concrete row "Co-authored-by: Name <a@b.co> trailing" rejected + subject decoy row before removing; parse_remote_config round trip delete OK.
4 dup deletions CONFIRMED (classify NUL reason wrong but covered).
5 quadratic input = "<"*N + ">b>"; dedupe quadratic too.
6 prop_assert: noise only. cases:64 does NOT override env (REFUTED) -> PROPTEST_CASES already works; FileFailurePersistence Off deliberate (keep or commit regs dir, decide).
7 merge tables: exact row lists recorded (version/classify/principal). DAG proptest: "principal on lane 0" only for first-parent chain; lanes<columns tautological; keep connected() asserts in fixed scenarios.

### V-settings restyle/theme
1 PARTIAL: body->label, help->small().weak(), link->hyperlink_to, field_font OK. interact_size.y=36 REFUTED (checkbox 36px, spinner, every horizontal row +8px) -> keep min_size on text_button. page_column styles don't reach bar/nav; TextStyle::Button unused by Button in 0.36 (Body); Name("section") panics outside; terminal preview exit message would grow 13->16. font_page(kind) needs ~8 params, saves ~25; selectable_row must take margin (3 copies incl changes.rs). section()/card() re-set item_spacing (free win). REAL estimate -120..-170 (not -350..-400).
2 Button floor CONFIRMED.
3 colors CONFIRMED w/ caveats: .strong().color(text_muted) at home.rs:194, history.rs:523 -> use .weak() and drop .strong(); depends on all states same text color. EXISTING BUG hover.rs:272-273 .monospace().small() -> small overrides monospace (hash drawn proportional).
4 REFUTED: only 7 PointingHand are Buttons; interact_cursor only Button; global cursor adds hand to branch-menu rows + Start shell again. corner_radius: 3 Buttons, 9 Frames (not reached); global change moves checkbox/textedit/etc from 2-3 to 6 -> visual change. => don't globalize.
5 POSSIBLE_LATER = roadmap on GitHub Auth page -> product decision (ask).
6 repo_view mostly CONFIRMED: progress_label exact; SetPanels absolute values ok (rewrite tests); bar_frame & House+Folder dup; commit_body merge PARTIAL (latest vs selected differ visibly -> keep two paths, share pieces); shown_oid() fn but recompute after history clicks; sync_control_width 1:1; sync_labeled color params constant. width measured with TextStyle::Button vs Body drawn.

### V-git exec/reap
1 capture structure CONFIRMED.
2 rewrite PARTIAL — must keep: after-exit path (wait 200ms, reap, wait, return Ok; background_writer_does_not_pin_a_finished_git); avoid recv_timeout Disconnected spin (keep own Sender); tag stdout/stderr messages & store; kill immediately on over-limit (incl stderr); DON'T kill early on read error (today -> TimedOut). Rewrite unit test a_read_error_is_not_an_empty_success.
3 expect on piped stdout OK.
4 NotADirectory: only remap the SPAWN failure (Spawn also covers try_wait/reader errors) -> spawn in caller, pass Child to capture.
5 reap PARTIAL: merge only group_leader + kill_descendants; terminal should_signal adds liveness check & lives in bin -> keep separate; seen redundant; gate exec.rs:585 + mod.rs:40 before #[cfg(unix)] module.
6 Git::read needs `what` param; glance.rs:84-88 differs (Ok(None)) -> leave.
7 trim_eol: THREE different rules (A: LF then CR always; B: CR only after LF; C: LF then CR only on Windows) -> one fn changes edge cases; keep per-rule or accept deliberately. glance redundant checks removable.
8 thiserror Path display OK; KEEP TimedOut.limit (tests read it).
9 repo.rs require_ok CONFIRMED; remotes classify_failure changes error type (invisible today).
10 branches CONFIRMED (name stays owned).
11 visibility: parse_remote_config must stay pub(crate) (author_tests uses); rest OK.

### V-github http/client
1 NotAllowed fixed message CONFIRMED and SAFER: current helper returns raw string when Uri parse fails (leaks creds/path/query in banner). Update patterns http.rs:353,424, adversarial_tests 189,340,358, avatars.rs:492 (Offline prod), :740.
2 read_capped take() CONFIRMED (reader yields decompressed); keep read_capped(reader, limit) signature; limit EOF quirk same.
3 zeroize PARTIAL: Token wipe must stay (release notes v0.0.6 promise) -> Token(Zeroizing<String>); keyring returned strings wipes real; remove env + sample-store wipes; forget_draft doesn't wipe (github_auth.rs:142) — gap.
4 dead API CONFIRMED w/ corrections: Token::new used by bin tests -> use Token::parse().expect; Candidates also; direct_avatar_url/gravatar_url must stay pub (bin tests).
5 UserBody/FetchError merge CONFIRMED; adversarial_tests.rs:964 checks Failed message -> rewrite.
6 allowed_host &'static str breaks .as_str() in tests (str::as_str unstable) — small edits; state() clones negligible; sha256_hex cosmetic.
7 KEEP redirect loop/Transport/sha2 CONFIRMED.
8 REFUTED: ureq-proto logs raw request bytes (incl Authorization) at TRACE; safe only because no logger installed -> add code comment + if ever adding a logger cap ureq/ureq_proto below TRACE (or log max_level feature). Consider `log` feature "release_max_level_debug"? (static guard).
9 Response<Vec<u8>> switch: NOT worth it (loses PartialEq, header semantics, coupling) -> SKIP.

### V-graph
1 keep algorithm CONFIRMED (drop vscode-git-graph comparison).
2 index rewrite PARTIAL: remaining == j>i for unique hashes CONFIRMED; risks: trunk.is_empty() meaning (need has_trunk bool); colors must be Vec<Option<usize>> during build; cycle guard only removable if trunk walk requires j>i (grafts honored!); store first_raw + kept arrays. Untested: no trunk/principal outside window, parent#1 out & #2 in, duplicate parents, tips outside window -> ADD TESTS FIRST, do separately.
3 BottomLane removal CONFIRMED (trace order matters for drawing; multi-lane exact order untested).
4 occupy PARTIAL: dead branches/seen/into_top confirmed; is_placeholder still used at 492/616.
5 by-value low value; skip or bundle.
6 estimate -50..-70 not -105. Safe subset: 3+4+mem::take first.

### V-terminal shell/keys
1 cfg(unix)/cfg(windows) CONFIRMED (CI runs tests on win+mac on PRs only; local Linux loses mac/win branches). Security: PTY cwd = repo; relative shell would run planted binary. NEW BUG: Windows program path unquoted in alacritty cmdline() -> "C:\Program Files\..." ambiguity (C:\Program.exe tried first) -> quote program.
2 px/clamp PARTIAL: +inf->65535; proposals equivalent; pty_size_is_safe never false (tests 2434/2439 rewrite); clamp also needed for to_winsize u16 multiply overflow (debug panic); Windows on_resize assert_eq S_OK. PERF: resize reads /proc every frame before same_window_size check -> reorder.
3 keys: Ctrl+C dead on Linux/Win, live on macOS but returns 0x03 same as letter_key -> delete OK. arrow==csi_tilde_or_letter; Home/End with app_cursor -> SS3 H/F (xterm). Key derives Ord, A–Z contiguous.
4 Selection CONFIRMED (better: anchor not rotated on scroll today); behavior change when output overwrites selected line; double-click on release.
5 fixed TERM low risk (loses Smulx, status line caps; xterm-256color advertises DECLRMM margins unsupported by vte -> possible misdraw). COLORTERM=truecolor set separately.
6 show/show_preview split feasible; surrender_focus no-op in preview; preview_line_count still used by settings.rs:782.
7 should_signal ALSO used on macOS by unix_child_is_alive -> resize (don't delete; dedupe with reap.rs). reap_process_tree used by exec.rs. Keep side join thread (dropping JoinHandle on egui thread may run child.wait()/ClosePseudoConsole there).
8 GridSize == term::test::TermSize (test module; keep own); row math duplicates point_to_viewport/viewport_to_point -> use them; terminfo_installed copies private fn (moot with fixed TERM).

### V-github/settings tests
- Locations: token_parse_output…, same_author…, huge_array, check tests in github/adversarial_tests.rs; builds_links in remote.rs; bodies_are_capped… in http.rs; settings proptests in git/proptest_tests.rs:459-658. Clippy -D warnings -> remove orphaned helpers.
- KEEP parse_trims_and_refuses_what_cannot_be_a_token (token.rs:786): only test of interior \n (header injection), non-ASCII, github_pat_ form.
- redirects_stay_inside_the_allowed_hosts holds ONLY TooManyRedirects assertion -> carry row; add exact 4-hop row (MAX_REDIRECTS pin).
- Proptests can't fail on host check -> add exact assert_eq! on ALLOWED_HOSTS; keep get_does_not_authorize as no-panic fuzz only; delete allowed_host_only_returns_allowlisted_hosts.
- client check table needs body column (403 secondary rate limit = only mentions_rate_limit test); keep "-1"->None and "nope"->Forbidden rows.
- settings: keep older_files_get_the_history_and_avatar_defaults, terminal_stays_hidden_unless_saved_open, missing_and_unknown_fields_are_tolerated, terminal_font_trims… (blank->null, padded cleaned); one_bad_column exhaustive loop 30 cases x good∈{20,1000}; key allowlist incl nested history_columns on save_to bytes.
- to_json/from_json don't exist; must include BOM strip + normalization; else recents coverage lost.
- Gaps to add: no raw-Unicode host row; ALLOWED_HOSTS not pinned; HttpError derived Debug prints URL creds; no test of user token vs error text; add check get() errors don't contain secret.
- adversarial_tests table: keep allowed_host_rejects_tricky_urls, redirect trio (or merge w/ TooManyRedirects), nulls, coauthor_without_authored_sha, pause_expires_at_reset, set_token_* (merge ok), noreply (+resolver row), hostile_author_avatar… (dedupe loops), token_parse_bounds (+rows); delete huge_array, same_author, allowed_host_only…, token_parse_output.

### V-UI shell
1 Task<T> PARTIAL: 8 single-result sites OK; NOT glance scan (streams, early stop), avatars pool, terminal. Keep send-before-request_repaint; per-site Disconnected mapping; tests need Task::ready/pending ctors (~35 lines). pick_folder() stays on UI thread.
2 map_err x11 CONFIRMED; stringify at poll point.
3 save_path CONFIRMED; keep a tested fn building path from load result (else failed_load test trivially passes).
4 SettingsScreen PARTIAL: page persists in ctx temp for session (survives Back/Home) -> keep page outside the Option; only merge preview with open flag.
5 home.rs: Facts CONFIRMED; only trailing fillers dead (331-333, 361-362), others hold layout; show_brand ScrollArea changes widget IDs + persisted scroll state -> skip.
6 Default derive OK. 7 Theme.name unused but documented (THEMES.md, dark.rs) -> update docs too. 8 process::exit -> return Ok OK.
9 AboutInfo PARTIAL: git_version is String -> AboutInfo<'a>/SettingsView<'a>; allocs only while Settings open — low value.
10 rfd threading CONFIRMED (comment accurate for macOS only).
11 comments: mod.rs 102; 14/30 why, 7 contracts, 5 narration, 4 WRONG/stale (173 cache vs config dir; 872 "fetch or pull" missing checkout; 124-125/684-685 scan stop timing; 160).

### V-log.rs
1 dead history() PARTIAL: more callers — adversarial_git_security.rs:740,1998 (alias.log/hooksPath/pager/fsmonitor checks); Commit is fixture type in ui tests; repo_states uses history() for NUL subject, SHA-256, shallow, packed-refs, usize::MAX. CAUTION: parse_graph_log fails WHOLE log on any bad record (graph.rs:344-348) vs parse_log drops trailing piece — moving tests to branch_graph may expose that a NUL in %s breaks the production graph (verify what git prints). free short_oid() is prod — keep.
2 CommitDetail/Hover merge CONFIRMED w/ small UI diffs (newline-only message; verbatim trailing blank lines); many fixtures to update; .stats -> method.
3 split_fields const-N CONFIRMED; 4th caller glance.rs:108.
4 is_oid CONFIRMED: accepts any non-empty hex (abbrev OK; also option-injection guard); more copies graph.rs:171, branches.rs:143 (allows empty), github/client.rs:180.
5 trailer sep merge CONFIRMED w/ rposition vs first difference; test helpers reorder.
6 parse_numstat naive split BREAKS renames -> stateful split iterator; keep digit-only (u64::from_str accepts "+5").
7 stats sum OK. 8 tag trim keep Unicode trim (U+00A0); test log.rs:1188 adjust.
9 DiffLine CONFIRMED; notice Option<&'static str> affects changes.rs:346 & fixture.
10 painted_lane_color only tests (doc misleading); SHORT_OID_LEN must stay pub (used by integration tests & bin) — REFUTED; email_key pub(crate) OK (doc link warning).
11 require_ok+rejected: more copies; message differs per call -> param; helper in exec.rs.
12 dedupe tips: git dedupes (first wins) CONFIRMED.
13 max-count clamp NEEDED (git 2.15 atoi wraps); keep, rewrite comment.

### V-errors/settings/token
1 thiserror PARTIAL: all messages reproducible; FetchError has NO Display (new messages needed); use #[error(transparent)] Http(#[from] HttpError) (avoid double print); SettingsError tuple fields need explicit #[source]. No test asserts full messages (substrings only).
2 serde defaults CONFIRMED (field default -> container __default -> error). Keep container #[serde(default)]; Default impl hard-codes true.
3 rename OK (rename reads+writes old key); many sites to update; SETTINGS.md:46 reword.
4 floor_char_boundary stable 1.91 CONFIRMED. 5 HashSet retain CONFIRMED.
6 sanitize_font_name PARTIAL: not idempotent (truncate leaves trailing space); ui/settings.rs:984-999 test expects sanitization -> keep or adapt.
7 late-save revert is INTENDED (v0.0.8 notes, H3); UI text is stale/contradictory ("Still waiting…" forever; also shown for finished Locked errors). Fix = behavior/text change -> separate bug-fix commit (UX), not refactor.
8 CTHULHU_KEYRING=sample in release CONFIRMED (cfg(not(test)) reads it); writes github_token_in_keyring:true; next launch "no longer in keychain". Removal = behavior change -> bug-fix commit.
9 REFUTED: save_to normalization is a tested public-API promise (4 assertions) -> KEEP.
10 ProjectDirs twice CONFIRMED (shared helper = public lib fn).
11 now_unix: i64 in hover vs u64 elsewhere -> u64 helper + cast.
6 avatar service KEEP CONFIRMED; image default Limits 512MiB alloc, no size cap; app decode stricter. Gap: EXIF orientation not applied (rotated JPEG avatars sideways).

## Baseline (mine)
- prod ~14k lines, tests ~26k. ~575 tests: lib 236, bin 166, integration 172.
- cargo test ~90 s wall; tests/adversarial_git_security.rs = 60 s of it.
- main is RED: ui::adversarial_tests::adversarial_missing_recent_shows_error_when_opened fails since "New home grid" (card says "Can't read this repository", test expects "is not a directory"). Because of fail-fast, integration tests didn't run in plain `cargo test`.
- clippy pedantic+nursery: ~464 warnings (casts, must_use, missing_errors_doc, pub(crate) in private mod, redundant_clone x12, needless_pass_by_value x15, needless_pass_by_ref_mut x11, match_same_arms x9, significant_drop_tightening x8).
- Context7 MCP: monthly quota reached (agents fell back to ~/.cargo/registry sources).

## UI history/inline tests agent
- 104 tests / ~4650 lines -> ~59 / ~2500. history_tests.rs 2301 -> ~1000.
- 7 selection tests -> 1 scripted harness; 4 copy tests -> 1 table; width sweep -> proptest on layout_columns in history.rs.
- Delete dead helper assert_titles_do_not_move (never runs), action_name exhaustive match -> derive(Debug).
- Mesh-vertex color inspection (lit halo) -> assert lit-branch state.
- Use egui_kittest built-ins (Node::click, hover_at, drag_at) instead of hand-rolled input pushers.
- terminal: merge shell_argv tests -> table; prompt_span x6 -> table; key encoding UNDER-tested (only Ctrl+L) -> add table; delete tautological ones.
- fonts: 544 -> ~220; avatars: 283 -> ~190; theme/main/repo_view/github_auth merges.
- Pure fns to extract: drag_columns/reset_column, empty_history, toggle_lit, special_key/encode_key, generic RequestCache<K,V> to unify HoverCache/DiffCache.
- pub(super) items exposed only for tests -> private.
- Shared #[cfg(test)] mod test_support in src/ui (fixture builder, label dump, proptest_config copied 4x).

## git log/graph agent (prod ~1645 lines -> -330..-400)
- Lane algorithm is standard (git graph.c/tig/vscode-git-graph) -> keep algorithm, rewrite representation.
- DEAD: log.rs history()/History/Commit/parse_log/parse_log_record/short_oid/latest (only tests use) -> delete + repoint tests to branch_graph/latest_commit. ~-85
- graph.rs: String-keyed maps/clones -> row indices (HashMap<&str,usize>, Vec<bool>, Lane::Waiting{next:usize} Copy, mem::take). drop remaining/kept_parents/cycle guard. keep raw first_parent. ~-40, med risk (exact traces tests protect).
- BottomLane reintroduces invalid state -> Vec<Lane>, push traces inline. ~-35
- occupy dead branches, double dedupe, impossible into_top==merges_into, is_placeholder. ~-30
- CommitDetail/CommitHover duplicate (formats + parsers) -> one parse_commit; CommitHover{commit,files,tags}; stats() method. ~-45
- head_commit_oid/parse_oid redundant git spawn (Head::Detached{oid} or latest.oid). ~-27
- layout_graph take Vec by value (C-CALLER-CONTROL).
- Parsing idioms: split_fields const-N array via TryFrom<Vec>, one is_oid (5 copies), one trailer sep const, parse_numstat via split, trim_ascii_end, quadratic parse_name_and_email -> rfind. ~-50
- Git::require_parsed helper (5 copies of require_ok+ok_or_else). ~-25
- test-only pub: painted_lane_color, fold_co_authors, CommitFile::diff_path -> delete; narrow pub.
- parse_file_diff: 5 DiffLine literals -> one push.
- Comment noise narrating past bugs/git versions.
## UI adversarial/network tests agent (62 tests/~4516 lines -> ~20/~1250)
- adversarial_tests.rs: 39 attrs = 28 scenarios; 11 parent/child re-exec pairs unix-only; child_* pass vacuously under --include-ignored.
- KEEP 3 (home actions, card text, branch menu disabled while git runs). MERGE into 4 tables (cli open -> repo/home error; browse disabled while busy; bottom bar with hostile names @520x400; branch menu checkout rows). Use Node::click_accesskit.
- MOVE to unit tests in mod.rs (no harness/threads/re-exec): persisting_actions_write_only_after_a_good_load (corrupt bytes, not chmod 0 — root in CI), home_keeps_last_repository_without_saving, poll_graph_applies_only_the_current_load (deletes lines 2157-2977 incl 185-line shell fake).
- DELETE: recent_cards_keep_the_same_size (pixels), history_titles_start... (dup), a_narrow_history... (no assert), thousand_commits (dup), adversarial_missing_recent_shows_error_when_opened (the RED one; only prints FINDING; covered by cli table row).
- network_tests.rs: no FakeTransport used. decode tests -> one table in avatars.rs; settings page actions -> table; proptests move to avatars.rs/hover.rs; delete 1.9M brute-force relative_time, pixel About layout, dup hover card test (runs real git on missing dir).
- PROD changes enabling this: (1) CthulhuApp::with_parts(ctx, cli, settings_path, avatars) instead of `if cfg!(test) { AvatarService::disabled() }` -> kills re-exec layer + 25-field struct literal in app_for_save; Windows coverage. (2) icon buttons get accessible name via widget_info (a11y bug!) -> removes ~250 lines of position-finding helpers. (3) apply_action(&mut self, ctx, action) extracted from mod.rs:600-671. (4) #[derive(Debug, PartialEq)] on Action -> delete describe_action/action_label/action_name copies.
- should_persist identity fn; checked_when_opened trivial; `#[path = "../../tests/common/mod.rs"] mod harness` pulls integration fixture into unit tests.
- Snapshot tests: not now (wgpu/dify, churn). Maybe 2-3 Linux-only later.
- Slow: settle_* sleep 100ms x200; fake git sleeps 2s; 11 child procs.

## lib unit tests agent (237 tests/~7150 lines -> ~116/~3500)
- No rstest/test-case/insta: plain `for (input, expected) in [..]` tables already house style.
- Dissolve git/author_tests.rs + git/proptest_tests.rs into owning modules; github/adversarial_tests.rs -> ~5 tests.
- DEAD prod: repo.rs parse_head is #[cfg(test)] parser (prod uses symbolic-ref) -> delete + its 7 tests + ~80 lines strategies.
- DELETE tests of std/serde/constants (derived Ord, Display strings, constants, directories crate, serde default), tests of test code, proptests whose oracle reimplements code (reference_co_authors ~110 lines, plausible token, remote trim), G1 characterization.
- MERGE tables: git_version_table, classify_failure_table, parse_log_table, parse_co_authors_table, graph principal/lane/color tables, fetch_request_table, discover PATH table, client check_table, redirect table, settings font sanitize table.
- graph DAG proptest replaces connected() checks in 11 tests.
- SEAMS: token.rs 26 cfg(test) items ~140 lines (SET_GATE, TIMEOUT_OVERRIDE, sample_store_requested...) -> trait SecretStore + Keychain<S> instance; prod OnceLock<Keychain<Box<dyn SecretStore>>>; tests use GatedFake; no global mutex. http.rs fake -> shared test support (binary can't see lib cfg(test) -> avatars.rs reimplements fakes; optional `test-support` feature). settings.rs: pure to_json/from_json; save_to/load_from thin I/O (proptests do ~256 fsyncs).
- src/test_support.rs (#[cfg(test)]): proptest_config, status_raw, OIDs, record builders, FakeTransport+helpers copied 3x.
- Proptest idioms: assert! -> prop_assert!; cases:64 hardcoded -> default + `.cargo/config.toml [env] PROPTEST_CASES="64"`; re-enable FileFailurePersistence + commit proptest-regressions/; drop mirror oracles (~200 lines); build URLs from parts.
- SECURITY: parse_name_and_email O(n²) on "<<<...<>" from untrusted trailers -> rfind O(n) (also noted by log agent).
- New test settings_json_has_only_known_keys (exact key allowlist) replaces weak !contains("token\":").

## github + settings prod agent (~2230 prod -> -690; plus -150 test hooks, -30 avatars.rs)
- token.rs:97-631 ~535 lines for 3 keychain calls (generation counter, cancelled/committed book, WorkerStatus+Condvar for tests, revert-on-late-save contradicting UI contract, SampleStore, CTHULHU_KEYRING=sample undocumented in release -> says "saved in keychain" but RAM only). -> one ordered worker thread (LazyLock<Option<Sender<Box<dyn FnOnce()+Send>>>>) + recv_timeout -> Locked; keyring_core::Entry; tests use keyring_core::mock::Store via set_default_store (keyring-core already in lock; add as direct dep). ~-490 prod. Med risk. (Alternative to lib-tests agent's SecretStore trait — keyring-core mock is the library-native seam: prefer it.)
- settings save_to hand-rolled temp+symlink+create_new+global save_lock; avatars.rs:427-476 copies it -> one write_atomic via tempfile::NamedTempFile::new_in + persist (tempfile -> normal dep). ~-65. File mode 0600.
- serde: container #[serde(default)] already uses Settings::default() -> remove field-level defaults/fns (comment is wrong). rename field + #[serde(rename)].
- Errors: hand-written Display/Error in http/client/token/settings; ApiError/FetchError/TokenStoreError don't impl Error -> thiserror derive, #[from]. ~-60
- url_without_credentials overkill -> NotAllowed unit/host. ~-25
- read_capped loop -> take(MAX+1).read_to_end (cap after decompression must stay). 
- zeroize theater (copies everywhere) -> Token(Zeroizing<String>), drop scattered calls.
- Dead pub: AVATAR_PX, USER_AGENT, CommitAccount(s), commit_accounts, parse_github_url, candidates, Token::new.
- UserBody dup GitHubUser; FetchError::Failed==Missing.
- truncate_to_bytes -> floor_char_boundary (1.91); normalize_recent HashSet retain; sanitize_font_name applied 3x again in ui -> enforce once.
- allowed_host -> &'static str; sha256_hex format! per byte; ui/settings.rs:669 parses+wipes env token EVERY FRAME.
- KEEP: Transport trait (single I/O seam, dyn ok), hand-rolled redirect loop (ureq has no per-hop hook), MAX_BODY after decompression, sha2 needed (Gravatar + cache names).

## git exec/discover/sync agent (~2180 prod -> -420..-470)
- exec.rs capture: 4 threads/git call, Arc<AtomicBool>, 55-line read_capped, CappedRead, 4 dup error blocks, finish_reader waits 2x200ms then discards -> readers send to one mpsc; take(MAX+1).read_to_end; recv_timeout(5ms) as poll + try_wait; kill & return w/o waiting readers; NotADirectory mapping into run_for; expect on piped stdout. ~-150, med risk (adversarial timeout/limit/setsid tests must pass unchanged).
- CORRECTNESS: reap_process_tree(pid) after try_wait reaped -> /proc walk can't find orphans; pid reuse could kill unrelated descendants -> kill_tree (alive) vs kill_group (after exit), or waitid(NOWAIT) via rustix to keep zombie. ~-15
- reap.rs: "safe to signal pid" check 3x (incl ui/terminal.rs should_signal) -> one signalable(); module #[cfg(unix)]; drop seen Vec. ~-35. KEEP /proc/ps walk (no unsafe, setsid requirement).
- Git::read<T>(cwd,args,parse) — 9 copies of rejected(format!("git {}"...)) (same as log agent's require_parsed). run_for formats cmdline even on success.
- trim_eol(&[u8])->&[u8] via strip_suffix — 7 copies allocating. glance redundant checks.
- Head::Detached{oid: full} + Display short -> drop head_commit_oid + UI detached_oid; 1 fewer git spawn/refresh.
- sync.rs: FetchRequest getters/SwitchRequest/SwitchKind test-only; dup char predicate; PULL_FF_ONLY_ARGS test compares const to itself -> fetch_refspec private + can_fetch(); switch_args. ~-40
- discover/exec cfg: 4 #[cfg] platform_fallbacks + 3 allow(dead_code) -> cfg!() branches; fold GUI PATH into command() (LazyLock). ~-45
- glance commit_glance ≈ log::latest_commit copy; date fmt 3x; is_local_stamp 20 lines. ~-70
- SECURITY (Windows): GIT_ prefix strip case-sensitive; Windows env case-insensitive -> eq_ignore_ascii_case. Drop GIT_OPTIONAL_LOCKS/GIT_PAGER env dup of flags. core.fsmonitor comment says test locks wrong value.
- thiserror Path display; Rejected{stderr}->reason; show limit.
- Visibility narrowing; remotes.rs builds GitError::Command by hand.
- OUT OF SCOPE notes: UI calls Git::discover() (git --version) on EVERY hover/diff/load/sync -> discover once, keep Git in app state. ui/terminal.rs:268 env::vars() panics on non-UTF-8 -> vars_os(). fetch splits upstream:short at first '/' (wrong for remotes with '/') -> typed Upstream{remote,branch}.
- KEEP: command() builder, inherited_git_var_is_cleared, DiscoverInputs injection, no git status on open, Head/Upstream enums.

## integration tests agent (179 tests/7325 lines -> ~41/~2400 + ~5 unit tests)
- Single integration crate tests/it/main.rs with modules support, discover, open, read, write, hardening, process, settings (Cargo book: each tests/*.rs = separate binary, run serially). One OnceLock fake git script with mode sidecar; cache Git::discover in OnceLock.
- hardening::hostile_repository_config_never_runs_on_reads (14->1: filters, aliases, include.path, hooksPath, pager, fsmonitor, textconv/extdiff; call every read entry point; log must be empty).
- hardening::hostile_inherited_environment_is_ignored (15->1 re-exec child with all hostile vars).
- write::pull_ff_only_never_rebases (7->1 + ff control); write::switch_attaches_head rows; switch_refuses_and_leaves_repo_unchanged rows.
- open/read tables; process: timeout_kills_the_process_group via run_for(..,300ms) (currently 2 tests take 50-60s!), output_cap, slow probe.
- Unit moves: command_with_env(program, parent_env) + get_envs(); argv prefix via get_args(); safe_directory_suggestion via sh function; override_spawn_path table.
- DELETE: dup of unit tests, tests of git/OS/serde behavior, tautological "no panic" ones, characterizations (B4 asserts current cost with ==, G6 nfd), macos_git_discovery.rs file removed.
- Re-exec stays for ONE e2e (edition 2024 set_var unsafe + forbid unsafe). Add clippy disallowed-methods for std::process::Command::new as static guard.
- Rename "adversarial" -> feature names; update ADVERSARIAL-TESTING.md Areas table, v0.0.8 report refs.
- No rstest/test-case. Keep proptest for parsers.
- concurrent_saves 1600 sync_all -> 2 bursts.

## UI terminal/history/hover agent (~5050 prod: -780 no-behavior-change; -1450 with options)
- BUG: terminal no set_focus_lock_filter -> Escape never reaches vim; Tab/arrows move focus. +3 lines + kittest.
- terminal Drop: ~200 lines of process-tree killing (/proc, ps, taskkill, join thread) -> alacritty Pty::drop SIGHUP; send Msg::Shutdown. DECISION: setsid/nohup jobs survive (normal terminal behavior). NOTE existing tests stop_session_kills_a_background_job_that_ignores_hup / dropped_shell_kills_a_job_that_left_the_session lock current behavior.
- HoverCache/DiffCache identical -> generic AsyncCache<K,V> with VecDeque. ~-80
- glow/crest mesh dup -> ribbon()/rings(). ~-80. OPTION: static highlight drops ~300 lines + 16ms repaint loop.
- OPTION: egui_extras::TableBuilder replaces hand column layout (-220 history, -60 HistoryColumns settings); widths absolute in egui memory; UX change; adds dep.
- terminal colors: 28-arm named_color etc -> resolve_color; BUG OSC 10/11 queries answered black. ~-50
- HostOs/this_host/injected closures to test every OS everywhere -> #[cfg(unix)]/#[cfg(windows)] fns. ~-80
- hand-made splitter -> egui::Panel::bottom resizable (0.36 API). ~-45
- selected commit / lit branch in egui temp memory -> OpenedRepo fields. ~-40
- author card hover rect -> egui Tooltip (stays open w/ interactive). ~-25 med risk.
- px/clamp_cell_px/pty_size_is_safe redundant; wrong comment about casts. KEEP resize guard.
- keys: Ctrl+C branch dead (egui-winit -> Event::Copy); arrow dup; letter() 26-arm. POSSIBLE BUG modified F1-F4 send CSI 11;m~ vs xterm CSI 1;mP.
- alacritty Selection full use (Side, Semantic/Lines). 
- avatars: generation Arc<AtomicU64> -> u64; disabled/Offline only via cfg!(test); DiskCache::write dup save_to; now_unix dup.
- changes.rs: each diff line laid out twice (painted + invisible Label for tests) -> ui.put(Label::new(galley)).
- TextWrapping::truncate_at_width replaces elided wrap code.
- OPTIONS: settings preview runs real shell (static prompt -120 lines); terminfo probing -> fixed TERM (-45).
- KEEP: don't adopt egui_term (egui 0.35, worse); per-cell glyph painting; custom avatar service (decode limits good); show_rows virtualization; OSC52 disabled; paste sanitizing.

## UI shell/screens agent (~5240 prod -> -1300..-1500, + ~450 test lines obsolete)
- fonts.rs FontService 5 fields + 3 ctx.data flags to work around next-pass font apply -> set_fonts once in new() + on SetUiFont; terminal faces registered in monospace chain; terminal font = pure fn of setting. -180 prod, -450 tests.
- mod.rs stale-result checks unreachable (graph_generation, view_id, glance_paths) -> move graph_load/sync_job into OpenedRepo, glance scan into Screen::Home(HomeScan); drop counter, Next enum, was_home, etc. -150, med risk.
- Worker boilerplate x9 (mod.rs, github_auth, folder_picker, changes, hover) -> ~25-line Task<T> {spawn(ctx,f), poll()}. -90
- Git::discover once in new(); About reads git.version (deletes git_version/git_probe/ensure/poll). -45
- .map_err(|e| e.to_string()) x11 -> Result<_, GitError> + ?; stringify once.
- ui/settings.rs 978 lines: per-label .size/.color, 15 consts, 12 text helpers, 5 copy-paste nav ifs, Appearance≈Terminal pages, specimen_row≈nav_button -> scoped Style text_styles in page_column, PAGES const loop, font_page(kind), selectable_row, AboutInfo &'static str. -350..-400. POSSIBLE_LATER roadmap in UI (owner call).
- Theme redundancy: 28 .color(palette.text) no-op, 40 .color(text_muted)==.weak(), 13 on_hover_cursor, 8 corner_radius -> visuals.interact_cursor + widgets.*.corner_radius. -80
- repo_view::show 225 lines, 6-tuple bool match, XOR toggles; bar_frame/nav cluster dup in settings.rs; paint_latest/paint_selected dup -> bottom_bar(), SyncKind::progress_label, Action::SetPanels(Panels), commit_body(). -150
- terminal splitter -> Panel::bottom resizable (agrees with terminal agent).
- OPTION icons: build.rs rasterizes 15 SVGs to raw RGBA + name list must match icons.rs + 2 15-arm matches + texture cache -> egui_extras svg/image loaders + include_image!. -140 but resvg in runtime binary (measure).
- icons.rs:187-224 min-size math works around egui stroke quirk -> retest after upgrade; image_tint_follows_text_color(true).
- build.rs git_executable defense (std Command doesn't search cwd), 2 sanitizers, hand VERSIONINFO -> ~20 lines; winresource or keep embed_resource. -110
- should_persist identity; loaded_ok+settings_path -> save_path: Option<PathBuf>.
- CthulhuApp::new(ctx, Startup{command_line, settings_path, avatar_cache}) (agrees w/ UI tests agent with_parts).
- settings_open/font_preview/page id in ctx.data -> settings: Option<SettingsScreen{page,preview}>.
- home.rs Facts dup; github_auth Default derive; now_unix x3; Theme.name unused; main process::exit(0) -> return.
- Comment noise ~380 lines (102 in mod.rs) -> keep only why. -150
- KEEP: theme::apply, Action Elm-style, Panel API, LayoutJob break_anywhere, rfd threading, custom JSON settings (not eframe::Storage).

## deps agent
- Breaking: eframe/egui/egui_kittest 0.36.2->0.37.0 (MSRV 1.95) L; rfd 0.17.2->0.18.0 S; resvg 0.48.1->0.50.0 S (+CI resvg CLI hashes); ico 0.5->0.6 S no code.
- Non-breaking: ureq 3.4.2->3.5.2 (limit now after gzip decode -> TooLarge becomes Transport; fix .limit(MAX_BODY+1) http.rs:172), proptest 1.12, pollster/serde_json/sha2/zeroize/rustix/embed-resource patch.
- Already latest: alacritty_terminal, directories, dunce, image, keyring, serde, thiserror, tempfile.
- egui 0.37: compile error history.rs:553 WidgetType::Other -> Role::Unknown; ~26 deprecations (WidgetType alias -> egui::Role; Id::new -> Id::unique (same hash) x18; ui.id() -> ui.scope_id()); kittest a11y check ON by default fails nameless buttons (icons.rs:128-133 -> .accessible_name(hover_text); repo_view.rs:428 Checkbox::without_text); kittest panics on missing glyphs (history_tests.rs:1022 字 -> allow_missing_glyphs); fonts: system_fonts default on in eframe, emoji behind monochrome_emoji_fonts (fix comments fonts.rs:3,353; keep system_fonts); password field hidden in accesskit (network_tests 637-643,786-790); tree/layout changes; panel IDs salted; set_fonts now applies immediately (=> FontService deferral unnecessary).
- All deps used; none replaceable by std. directories ProjectDirs built twice (settings.rs:208, ui/mod.rs:175). Add [lints.cargo] unused_dependencies="warn" (Cargo 1.100).
- Toolchain: 1.100.0 (2026-11-12). New std: str::trim_prefix/trim_suffix, bool::toggle, String::from_utf8_lossy_owned (1.99), mem::DropGuard.
- Actions: install-action v2.90.11 073e1fd0bc889c9aee68dbaaa927cde1b5653a83; upload-artifact v7.0.2 cf430e030ddbb5b0abf93d22962f4752f3646cd9; download-artifact v8.0.2 9000827ccba6bdab643e8b6fd33ac0654aef8333; softprops/action-gh-release v3.0.4 8a0cd651e5b2365bd640854a3fa5dfc70e4875f3 (or gh release create); zizmor 1.33.1; cargo-generate-rpm 0.21.1; resvg CLI 0.50.0 linux 5d1c9ffd43c82fb802d92cef8100055c612cd3e248d5a787c20079293a456e21 macos 8155b569f2b93723ba3645b53536eead3b591c8faf62bdaafd3033ba37838837. Keep ubuntu-24.04; pin macos-26, windows-2025.
- Order: CI pins -> cargo update + floors + ureq fix -> resvg/ico -> rfd -> toolchain 1.100 -> egui 0.37 (6a compile+deprecations, 6b kittest) -> optional 1.100 simplifications.

- BUGS: (1) parse_file_diff submodule/binary checks scan diff content ("+price 160000" -> "Submodule update") -> check header only. (2) graph.rs L413 placeholder pushed when main history ended (orphan branch older than main root) -> verify.

## PHASE 0 EXPERIMENTS (2026-11-26)
- NUL in subject through branch_graph: Ok — git's %s stops at NUL ("subject with NUL"). NOT a bug. history() tests can be repointed safely.
- PROPTEST_CASES env OVERRIDES explicit `cases: 64` (64 / 1 / 500 observed). No [env] config needed; deep runs via PROPTEST_CASES=N already work.
- git 2.53: `pull.rebase=preserve` with app's `--no-rebase` -> git doesn't read pull.rebase; pull refused for divergence (correct reason). Without --no-rebase -> fatal invalid value. Row meaningful.
- Shell note: `rm` is interactive alias in user's zsh -> use `command rm -f`.
