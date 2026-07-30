#!/bin/sh
# mars-doc-version: 4
#
# How the manager agent runs. Mars only decides WHEN there is work; everything about HOW the
# agent runs lives here, so it can be changed with an editor instead of a rebuild.
#
#   sh ~/.mars/manager/run.sh     # run one turn right now, no daemon and no waiting
#
# ANTHROPIC_API_KEY is unset deliberately: when it is present `claude` prefers it over the
# claude.ai login, which bills credits instead of the subscription — and fails outright when that
# key is empty. --add-dir is required because session artifacts live outside this directory.
# acceptEdits lets it write with nobody at the keyboard; `auto` would also permit classifier-gated
# shell, which is a wider surface than an agent reading untrusted terminal output should have.
cd "$(dirname "$0")" || exit 1

exec env -u ANTHROPIC_API_KEY -u ANTHROPIC_AUTH_TOKEN \
  claude \
    --model claude-sonnet-5 \
    --effort medium \
    --add-dir "$HOME/.mars/sessions" \
    --permission-mode acceptEdits \
    -p "$(cat prompt.md 2>/dev/null || echo 'Warm-up run — process the open batch in inbox/ per AGENTS.md')"
