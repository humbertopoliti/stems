@FR-SC-1 @FR-LC-2
Feature: pre_start, post_start, pre_stop and post_stop hooks
  Hooks run around the process: `pre_start` before `starting`,
  `post_start` once healthy, `pre_stop` before the process is signalled and
  `post_stop` once it is gone. These hooks set `cwd: workspace`, so they run
  in the integration repo and append to hooks.log there.

  Scenario: hooks run in order around start and stop
    Given the fixture workspace "scripts-hooks"
    When I run "stems up --detach --json"
    Then the command succeeds
    And the file "hooks.log" contains "pre_start echo-svc"
    And the file "hooks.log" contains "post_start port=${port:18521}"
    And the events stream contains {"kind": "script.finished", "stem": "echo-svc", "data": {"script": "pre_start", "exit": 0}} before {"kind": "stem.state", "stem": "echo-svc", "to": "starting"}
    And the events stream contains {"kind": "stem.state", "stem": "echo-svc", "to": "healthy"} before {"kind": "script.started", "stem": "echo-svc", "data": {"script": "post_start"}}
    When I run "stems stop echo-svc --json"
    Then the command succeeds
    And the file "hooks.log" contains "post_stop"
    And the events stream contains {"kind": "stem.state", "stem": "echo-svc", "to": "stopping"} before {"kind": "script.started", "stem": "echo-svc", "data": {"script": "pre_stop"}}
    And the events stream contains {"kind": "script.finished", "stem": "echo-svc", "data": {"script": "pre_stop"}} before {"kind": "script.started", "stem": "echo-svc", "data": {"script": "post_stop"}}
    And the events stream contains {"kind": "script.finished", "stem": "echo-svc", "data": {"script": "post_stop"}} before {"kind": "stem.state", "stem": "echo-svc", "to": "stopped"}
    When I run "stems down --all --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive
