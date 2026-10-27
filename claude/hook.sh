#!/bin/sh
# Claude Code hook, for all the events of the claude plugin: sends the hook
# input to the plugins of the zellij session, with the pane of Claude and the
# time. In the background: never slows down nor fails Claude Code, and writes
# nothing to stdout (added to the context of Claude).
input=$(cat)

[ -n "$ZELLIJ" ] && [ -n "$ZELLIJ_PANE_ID" ] || exit 0

zellij pipe --name claude::event --args "pane_id=$ZELLIJ_PANE_ID,time=$(date +%s)" -- "$input" \
  < /dev/null > /dev/null 2>&1 &

exit 0
