@FR-LC-5 @FR-ST-3
Feature: Processes get the configured environment and working directory
  A process spec's `cwd` and `env` reach the process; its stdout and stderr
  arrive as `process.output` events, line by line.

  Background:
    Given the "minimal" workspace
    And the daemon is started with debug RPCs

  Scenario: cwd and env are applied and both output streams are captured
    When I call the daemon RPC "_debug.start_raw" with {"spec": {"command": "pwd; echo \"FOO=$FOO\"; echo oops >&2", "shell": true, "cwd": "${ws}", "env": {"FOO": "bar"}}}
    Then the command succeeds
    And within 5s the events stream contains {"kind": "process.output", "data": {"stream": "stdout", "text": "${ws}"}}
    And within 5s the events stream contains {"kind": "process.output", "data": {"stream": "stdout", "text": "FOO=bar"}}
    And within 5s the events stream contains {"kind": "process.output", "data": {"stream": "stderr", "text": "oops"}}
    And within 5s the events stream contains {"kind": "process.exited", "data": {"code": 0}}

  Scenario: clear_env starts from an empty environment
    When I call the daemon RPC "_debug.start_raw" with {"spec": {"command": "echo \"HOME=$HOME FOO=$FOO\"", "shell": true, "cwd": "${ws}", "env": {"FOO": "only"}, "clear_env": true}}
    Then the command succeeds
    And within 5s the events stream contains {"kind": "process.output", "data": {"text": "HOME= FOO=only"}}
