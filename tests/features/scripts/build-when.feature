@FR-SC-1
Feature: build can run in the start sequence (when: changed | always)
  A stem's `build` is on demand by default. With `when: changed` it also
  runs in every start, after `setup` and before `pre_start`, stamped like
  `setup` (inputs, stamp_env); with `when: always` it runs on every start.

  Scenario: when changed runs after setup, before pre_start, and only when its stamp changes
    Given the fixture workspace "scripts-build-when"
    And the workspace has a private copy of the repos
    And the file "${tmp}/examples/repos/shop-api/VERSION" is written with "1.0.0"
    When I run "stems up changed --detach --json"
    Then the command succeeds
    And the events stream contains {"kind": "script.started", "stem": "changed", "data": {"script": "setup"}} before {"kind": "script.started", "stem": "changed", "data": {"script": "build"}}
    And the events stream contains {"kind": "script.finished", "stem": "changed", "data": {"script": "build", "exit": 0}} before {"kind": "script.started", "stem": "changed", "data": {"script": "pre_start"}}
    And the events stream contains {"kind": "script.started", "stem": "changed", "data": {"script": "pre_start"}} before {"kind": "stem.state", "stem": "changed", "to": "starting"}
    When I run "stems stamps --json"
    Then the command succeeds
    And the JSON nodes at "$.data.stamps[*].script" equal ["build", "setup"]
    # A new daemon with a current stamp: build does not run.
    When I run "stems down --json"
    Then the command succeeds
    When I run "stems up changed --detach --json"
    Then the command succeeds
    And within 5s the events stream contains {"kind": "stem.state", "stem": "changed", "to": "healthy"}
    And the events stream does not contain {"kind": "script.started", "data": {"script": "build"}}
    # An input change re-runs it.
    When I run "stems down --json"
    Then the command succeeds
    Given the file "${tmp}/examples/repos/shop-api/VERSION" is written with "1.0.1"
    When I run "stems up changed --detach --json"
    Then the command succeeds
    And within 5s the events stream contains {"kind": "script.finished", "stem": "changed", "data": {"script": "build", "exit": 0}}
    When I run "stems down --all --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive

  Scenario: when always runs on every start and the default never runs on up
    Given the fixture workspace "scripts-build-when"
    When I run "stems up always plain --detach --json"
    Then the command succeeds
    And the events stream contains {"kind": "script.finished", "stem": "always", "data": {"script": "build", "exit": 0}} before {"kind": "stem.state", "stem": "always", "to": "starting"}
    And the events stream does not contain {"kind": "script.started", "stem": "plain", "data": {"script": "build"}}
    When I run "stems restart always --json"
    Then the command succeeds
    # `restart --build` runs the build once, not again in the start sequence.
    When I run "stems restart always --build --json"
    Then the command succeeds
    When I run "stems stamps --json"
    Then the JSON at "$.data.stamps" equals []
    When I run "stems logs always --script build --json"
    Then the JSON nodes at "$[*].text" equal ["built", "built", "built"]
    # Explicit build still works for an on-demand build.
    When I run "stems build plain --json"
    Then the command succeeds
    When I run "stems down --all --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive

  Scenario: when is refused on scripts other than build
    Given the fixture workspace "scripts-build-when"
    And a local override setting stems.plain.scripts.setup={command: echo hi, when: always}
    When I run "stems validate --json"
    Then the exit code is 2
    And the JSON error has code "SCHEMA_INVALID" and path "stems.plain.scripts.setup.when"
    And the error message contains "when applies only to a stem's build script"
