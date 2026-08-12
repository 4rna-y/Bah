# Agent notes

## Runtime logs

Bah writes its application log to standard output and also appends it to
`$XDG_STATE_HOME/bah/bah.log`. When `XDG_STATE_HOME` is unset, use
`~/.local/state/bah/bah.log`. `BAH_LOG_FILE` overrides this location.

Before diagnosing a Bah runtime issue, inspect the latest entries in this file
with `tail -n 200 "$XDG_STATE_HOME/bah/bah.log"` (or the fallback path), then
correlate them with the user-reported action and timestamp.
