# Read by the home screen (apps/lucina-app/src/manifest.rs). scripts/gui-initramfs.sh installs it.
# CodeCor: the coding agents as a native app. The local Qwen mode runs in the terminal, as `lucina-app-coder --terminal <dir>`,
# which the shell types there (the control socket's `type` request). The intents are `<id>.<verb>`, as the manifest
# requires, so the app's id is codecor while the program stays lucina-app-coder.
id = codecor
title = CodeCor
icon = terminal
needs_network = true
exec = /usr/bin/lucina-app-coder
intents = codecor.projects, codecor.open, codecor.agents, codecor.new_agent, codecor.send, codecor.status
