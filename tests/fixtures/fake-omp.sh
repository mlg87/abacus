#!/bin/sh
# Stand-in for `omp -p --mode json …` in end-to-end smoke runs: emits the event
# shapes abacus parses, paced by FAKE_SLEEP, exiting FAKE_EXIT. Arguments ignored.
# printf, not echo: POSIX sh echo expands the JSON's literal \n escapes.
pause="${FAKE_SLEEP:-3}"
code="${FAKE_EXIT:-0}"

emit() { printf '%s\n' "$1"; }

emit '{"type":"session","version":3,"id":"fake-session-0001"}'
sleep "$pause"
emit '{"type":"tool_execution_start","toolCallId":"t1","toolName":"bash","intent":"Running fake step"}'
sleep "$pause"
emit '{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"Did the fake work.\nRESULT: none — fake run"}],"usage":{"cost":{"total":0.05}}}}'
sleep "$pause"
emit '{"type":"agent_end","messages":[]}'

if [ "$code" != "0" ]; then
  echo "fake failure" >&2
fi
exit "$code"
