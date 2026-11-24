# Glossary

| nnn term | Meaning | Rust name |
|---|---|---|
| context | one of 4 tabs, each with its own dir and settings | `Context` |
| dent / entry | a directory entry shown in the list | `Entry` |
| selection (selbuf) | NUL-separated list of selected paths, written to ~/.config/nnn/.selection | `Selection` |
| session | saved contexts in ~/.config/nnn/sessions/<name> | `Session` |
| picker mode | -p: selection written to file/stdout on quit | `picker` |
| bookmark (NNN_BMS) | key:path pairs | `Bookmarks` |
| plugin (NNN_PLUG) | key:script pairs run from ~/.config/nnn/plugins | `Plugins` |
| list mode | paths read from stdin into a temp dir | `listpath` |
