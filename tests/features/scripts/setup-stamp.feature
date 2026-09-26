@FR-SC-1 @FR-SC-5 @FR-LC-1
Feature: setup runs only when its stamp changes
  A stem's `setup` script is stamped with sha256(script text + contents of
  its `inputs` + `stamp_env` values), stored in state.json and carried from
  daemon run to daemon run. `up` runs setup (state `setup`, before
  `starting`) only when the stamp is missing or changed; `up --fresh` runs
  `reset`, clears the stamps and so runs setup again.

  Scenario: setup runs once, again after an input changes, and after --fresh
    Given the fixture workspace "scripts-stamp"
    And the workspace has a private copy of the repos
    And the file "${tmp}/examples/repos/shop-api/VERSION" is written with "1.0.0"
    When I run "stems up --detach --json"
    Then the command succeeds
    And the events stream contains {"kind": "stem.state", "stem": "echo-svc", "to": "setup"} before {"kind": "stem.state", "stem": "echo-svc", "to": "starting"}
    And the events stream contains {"kind": "script.finished", "stem": "echo-svc", "data": {"script": "setup", "exit": 0}} before {"kind": "stem.state", "stem": "echo-svc", "to": "starting"}
    When I run "stems logs echo-svc --script setup --json"
    Then the JSON nodes at "$..text" equal ["setup-ran"]
    When I run "stems stamps --json"
    Then the command succeeds
    And the JSON at "$.data.stamps[0].stem" equals "echo-svc"
    And the JSON at "$.data.stamps[0].script" equals "setup"
    And the JSON at "$.data.stamps[0].inputs" equals ["VERSION"]
    # A new daemon: the stamp is current, setup does not run.
    When I run "stems down --json"
    Then the command succeeds
    And the JSON at "$.data.daemon_stopping" equals true
    When I run "stems up --detach --json"
    Then the command succeeds
    And within 5s the events stream contains {"kind": "stem.state", "stem": "echo-svc", "to": "healthy"}
    And the events stream does not contain {"kind": "script.started", "data": {"script": "setup"}}
    And the events stream does not contain {"kind": "stem.state", "to": "setup"}
    # Offline, `stamps` reads state.json.
    When I run "stems down --json"
    Then the command succeeds
    When I run "stems stamps echo-svc --json"
    Then the command succeeds
    And the JSON at "$.data.stamps[0].script" equals "setup"
    # Changing an input changes the stamp.
    Given the file "${tmp}/examples/repos/shop-api/VERSION" is written with "1.0.1"
    When I run "stems up --detach --json"
    Then the command succeeds
    And within 5s the events stream contains {"kind": "script.finished", "stem": "echo-svc", "data": {"script": "setup", "exit": 0}}
    # --fresh (on a new daemon, so the events are its own): reset, clear
    # stamps, setup again.
    When I run "stems down --json"
    Then the command succeeds
    When I run "stems up --detach --fresh --json"
    Then the command succeeds
    And the events stream contains {"kind": "script.finished", "stem": "echo-svc", "data": {"script": "reset", "exit": 0}} before {"kind": "stem.state", "stem": "echo-svc", "to": "setup"}
    And the events stream contains {"kind": "up.started", "data": {"fresh": true}} before {"kind": "script.started", "stem": "echo-svc", "data": {"script": "reset"}}
    When I run "stems status --json"
    Then the JSON at "$.data.stems[0].state" equals "healthy"
    When I run "stems down --all --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive

  Scenario: stems reset stops the stem, runs reset and clears its stamps
    Given the fixture workspace "scripts-stamp"
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run "stems reset echo-svc --json"
    Then the exit code is 2
    And the JSON error has code "DESTRUCTIVE_NOT_CONFIRMED"
    When I run "stems reset echo-svc --yes --json"
    Then the command succeeds
    And the JSON at "$.data.stopped" equals ["echo-svc"]
    And the JSON at "$.data.reset[0].script" equals "reset"
    And the JSON at "$.data.cleared" equals ["echo-svc"]
    When I run "stems stamps --json"
    Then the JSON at "$.data.stamps" equals []
    When I run "stems status --json"
    Then the JSON at "$.data.stems[0].state" equals "stopped"
    When I run "stems up --detach --json"
    Then the command succeeds
    And within 5s the events stream contains {"kind": "script.finished", "stem": "echo-svc", "data": {"script": "setup"}}
    When I run "stems down --all --json"
    Then the command succeeds

  Scenario: build runs the build script, and restart --build runs it before starting
    Given the fixture workspace "scripts-stamp"
    When I run "stems build --json"
    Then the command succeeds
    And the JSON at "$.data.built[0].stem" equals "echo-svc"
    And the JSON at "$.data.built[0].exit" equals 0
    And the lock file and socket do not exist
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run "stems restart echo-svc --build --json"
    Then the command succeeds
    And the JSON at "$.data.ready" equals ["echo-svc"]
    And the events stream contains {"kind": "stem.state", "stem": "echo-svc", "to": "stopped"} before {"kind": "script.started", "stem": "echo-svc", "data": {"script": "build"}}
    When I run "stems logs echo-svc --script build --json"
    Then the JSON at "$..text" contains "build-ran"
    When I run "stems down --all --json"
    Then the command succeeds
