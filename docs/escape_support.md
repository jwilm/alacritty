# Escape Sequence Support

This list includes all escape sequences Alacritty currently supports.

### Legend

The available statuses are `PARTIAL`, `IMPLEMENTED` and `REJECTED`. While a
status of `PARTIAL` means there is still work left to be done, a status of
`IMPLEMENTED` for something partially implemented means all other features were
rejected.

All whitespace in escape sequences is solely for formatting purposes and all
relevant spaces are denoted as `SP`. The escape parameters are omitted for
brevity.

### ESC codes - `ESC`

| ESCAPE    | STATUS      | NOTE                                               |
| --------- | ----------- | -------------------------------------------------- |
| `ESC (`   | IMPLEMENTED | Only charsets `B` and `0` are supported            |
| `ESC )`   | IMPLEMENTED | Only charsets `B` and `0` are supported            |
| `ESC *`   | IMPLEMENTED | Only charsets `B` and `0` are supported            |
| `ESC +`   | IMPLEMENTED | Only charsets `B` and `0` are supported            |
| `ESC =`   | IMPLEMENTED |                                                    |
| `ESC >`   | IMPLEMENTED |                                                    |
| `ESC 7`   | IMPLEMENTED |                                                    |
| `ESC 8`   | IMPLEMENTED |                                                    |
| `ESC # 8` | IMPLEMENTED |                                                    |
| `ESC D`   | IMPLEMENTED |                                                    |
| `ESC E`   | IMPLEMENTED |                                                    |
| `ESC H`   | IMPLEMENTED |                                                    |
| `ESC M`   | IMPLEMENTED |                                                    |
| `ESC Z`   | IMPLEMENTED |                                                    |

### CSI (Control Sequence Introducer) - `ESC [`

| ESCAPE     | STATUS      | NOTE                                              |
| ---------- | ----------- | ------------------------------------------------- |
| ``CSI ` `` | IMPLEMENTED |                                                   |
| `CSI @`    | IMPLEMENTED |                                                   |
| `CSI A`    | IMPLEMENTED |                                                   |
| `CSI a`    | IMPLEMENTED |                                                   |
| `CSI B`    | IMPLEMENTED |                                                   |
| `CSI b`    | IMPLEMENTED |                                                   |
| `CSI C`    | IMPLEMENTED |                                                   |
| `CSI c`    | IMPLEMENTED |                                                   |
| `CSI D`    | IMPLEMENTED |                                                   |
| `CSI d`    | IMPLEMENTED |                                                   |
| `CSI E`    | IMPLEMENTED |                                                   |
| `CSI e`    | IMPLEMENTED |                                                   |
| `CSI F`    | IMPLEMENTED |                                                   |
| `CSI f`    | IMPLEMENTED |                                                   |
| `CSI G`    | IMPLEMENTED |                                                   |
| `CSI g`    | IMPLEMENTED |                                                   |
| `CSI H`    | IMPLEMENTED |                                                   |
| `CSI h`    | PARTIAL     | Only modes `4` and `20` are supported             |
| `CSI ? h`  | PARTIAL     | Supported modes:                                  |
|            |             |   `1`, `3`, `6`, `7`, `12`, `25`, `1000`, `1002`  |
|            |             |   `1004`, `1005`, `1006`, `1007`, `1042`, `1049`  |
|            |             |   `2004`                                          |
| `CSI I`    | IMPLEMENTED |                                                   |
| `CSI J`    | IMPLEMENTED |                                                   |
| `CSI K`    | IMPLEMENTED |                                                   |
| `CSI L`    | IMPLEMENTED |                                                   |
| `CSI l`    | PARTIAL     | See `CSI h` for supported modes                   |
| `CSI ? l`  | PARTIAL     | See `CSI ? h` for supported modes                 |
| `CSI M`    | IMPLEMENTED |                                                   |
| `CSI m`    | PARTIAL     | Only singular straight underlines are supported   |
| `CSI n`    | IMPLEMENTED |                                                   |
| `CSI P`    | IMPLEMENTED |                                                   |
| `CSI SP q` | IMPLEMENTED |                                                   |
| `CSI r`    | IMPLEMENTED |                                                   |
| `CSI S`    | IMPLEMENTED |                                                   |
| `CSI ? S`  | PARTIAL     | Only for reading attributes.                      |
| `CSI s`    | IMPLEMENTED |                                                   |
| `CSI T`    | IMPLEMENTED |                                                   |
| `CSI t`    | PARTIAL     | Only parameters `22` and `23` are supported       |
|            | REJECTED    | `1`-`13`, `15`, `19`-`21`, `24`                   |
| `CSI u`    | IMPLEMENTED |                                                   |
| `CSI X`    | IMPLEMENTED |                                                   |
| `CSI Z`    | IMPLEMENTED |                                                   |

### OSC (Operating System Command) - `ESC ]`

| ESCAPE    | STATUS      | NOTE                                               |
| --------- | ----------- | -------------------------------------------------- |
| `OSC 0`   | IMPLEMENTED | Icon names are not supported                       |
| `OSC 1`   | REJECTED    | Icon names are not supported                       |
| `OSC 2`   | IMPLEMENTED |                                                    |
| `OSC 4`   | IMPLEMENTED |                                                    |
| `OSC 10`  | IMPLEMENTED |                                                    |
| `OSC 11`  | IMPLEMENTED |                                                    |
| `OSC 12`  | IMPLEMENTED |                                                    |
| `OSC 50`  | IMPLEMENTED | Only `CursorShape` is supported                    |
| `OSC 52`  | IMPLEMENTED | Only Clipboard and primary selection supported     |
| `OSC 104` | IMPLEMENTED |                                                    |
| `OSC 110` | IMPLEMENTED |                                                    |
| `OSC 111` | IMPLEMENTED |                                                    |
| `OSC 112` | IMPLEMENTED |                                                    |

### DCS (Device Control String) - `ESC P`

| ESCAPE    | STATUS      | NOTE                                               |
| --------- | ----------- | -------------------------------------------------- |
| `DCS = s` | IMPLEMENTED |                                                    |
| `DCS q`   | IMPLEMENTED |                                                    |
