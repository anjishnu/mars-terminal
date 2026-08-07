#!/bin/sh
# mars-doc-version: 5
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

# The agent may not edit its OWN instructions. It reads untrusted terminal output, and text on a
# screen that can rewrite the standing orders turns one bad tick into a permanent condition — the
# prompt is re-read on every run, so an edit here outlives the run that made it. AGENTS.md already
# says "never edit them"; this is that sentence enforced instead of requested.
#
# The circularity is deliberate and closed: these flags live in run.sh, and editing run.sh stops
# the agent entirely (Mars compares it to the built-in and refuses unblessed drift). Removing the
# guard requires defeating the guard on the file that carries it.
exec env -u ANTHROPIC_API_KEY -u ANTHROPIC_AUTH_TOKEN \
  claude \
    --model claude-sonnet-5 \
    --effort medium \
    --add-dir "$HOME/.mars/sessions" \
    --permission-mode acceptEdits \
    --disallowedTools \
      'Edit(AGENTS.md)' 'Edit(prompt.md)' 'Edit(policy.md)' \
      'Edit(run.sh)' 'Edit(docs/**)' 'Edit(.claude/**)' \
    -p "$(cat prompt.md 2>/dev/null || echo 'Warm-up run — process the open batch in inbox/ per AGENTS.md')"
