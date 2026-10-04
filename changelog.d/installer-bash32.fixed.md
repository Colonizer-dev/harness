- **The installers parse again under macOS `/bin/sh` (bash 3.2).** The app-slot process walk added
  in 0.2.1's update draining put a `case` inside `$( … )`, which bash 3.2 cannot parse: `curl … |
  sh` on macOS and `scripts/install.sh --bundle` stopped with `syntax error near unexpected token
  ';;'` before doing anything. The pattern now carries its optional leading `(`, which every POSIX
  shell accepts.
