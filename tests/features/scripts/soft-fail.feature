@FR-SC-5 @FR-HS-2
Feature: soft_fail lifecycle scripts degrade their stem instead of failing it
  Exit 0 means done (stamped); non-zero means not done. With `soft_fail:
  true` a failing `setup`/`seed`/... does not fail the stem: the start goes
  on, nothing is stamped (the script runs again next `up`), and the stem is
  `healthy` but degraded with a warning until it is stopped or restarted. A
  soft-failed `seed` leaves `seeded: false`, so `condition: seeded`
  dependants are skipped. `stems run` of such a script still fails.

  Scenario: a soft-failing setup degrades the stem, is never stamped and runs again
    Given the fixture workspace "scripts-soft-fail"
    When I run "stems up api --detach --json"
    Then the command succeeds
    And the JSON at "$.data.ready" equals ["api"]
    And the JSON at "$.data.degraded[0]" equals {"stem": "api", "script": "setup", "exit": 3}
    And the events stream contains {"kind": "script.finished", "stem": "api", "data": {"script": "setup", "exit": 3, "ok": false, "soft": true}} before {"kind": "stem.state", "stem": "api", "to": "starting"}
    And the events stream contains {"kind": "stem.degraded", "stem": "api", "data": {"script": "setup", "exit": 3}} before {"kind": "stem.state", "stem": "api", "to": "starting"}
    When I run "stems status api --json"
    Then the JSON at "$.data.stems[0].state" equals "healthy"
    And the JSON at "$.data.stems[0].degraded" equals true
    And the JSON at "$.data.stems[0].glyph" equals "degraded"
    And the JSON at "$.data.stems[0].reason" equals "setup exited 3 (soft_fail)"
    And the JSON at "$.data.stems[0].warnings[0].script" equals "setup"
    And the JSON at "$.data.stems[0].warnings[0].exit" equals 3
    And the JSON at "$.data.stems[0].warnings[0].tail" equals ["poetry missing; skipping"]
    And the JSON at "$.data.summary.degraded" equals 1
    When I run "stems stamps --json"
    Then the JSON at "$.data.stamps" equals []
    # A restart clears the warning, re-runs setup and degrades the stem again.
    When I run "stems restart api --json"
    Then the command succeeds
    And the JSON at "$.data.degraded[0].script" equals "setup"
    When I run "stems logs api --script setup --json"
    Then the JSON nodes at "$[*].text" equal ["poetry missing; skipping", "poetry missing; skipping"]
    When I run "stems status api --json"
    Then the JSON at "$.data.stems[0].warnings[0].script" equals "setup"
    # A stop clears it.
    When I run "stems stop api --json"
    Then the command succeeds
    When I run "stems status api --json"
    Then the JSON at "$.data.stems[0].warnings" does not exist
    And the JSON at "$.data.stems[0].degraded" equals false
    # The next up (a new daemon) runs setup again: it was never stamped.
    When I run "stems down --all --json"
    Then the command succeeds
    When I run "stems up api --detach --json"
    Then the command succeeds
    And within 5s the events stream contains {"kind": "script.started", "stem": "api", "data": {"script": "setup"}}
    # An explicit run of the script fails: softness is for the lifecycle.
    When I run "stems run api setup --json"
    Then the exit code is 1
    And the JSON at "$.data.ok" equals false
    And the JSON at "$.data.soft" equals true
    And the JSON at "$.data.exit" equals 3
    When I run "stems down --all --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive

  Scenario: a soft-failing seed leaves the stem unseeded and skips its seeded dependants
    Given the fixture workspace "scripts-soft-fail"
    When I run "stems up db web --detach --json"
    Then the exit code is 3
    And the JSON at "$.data.ready" equals ["db"]
    And the JSON at "$.data.skipped" equals ["web"]
    And the JSON at "$.data.skipped_reasons.web" equals "seed_degraded"
    And the JSON at "$.data.degraded[0]" equals {"stem": "db", "script": "seed", "exit": 4}
    When I run "stems status --json"
    Then the JSON at "$.data.stems[?@.name=='db'].state" equals "healthy"
    And the JSON at "$.data.stems[?@.name=='db'].seeded" equals false
    And the JSON at "$.data.stems[?@.name=='db'].degraded" equals true
    And the JSON at "$.data.stems[?@.name=='db'].reason" equals "seed exited 4 (soft_fail)"
    And the JSON at "$.data.stems[?@.name=='web'].state" equals "stopped"
    And the events stream does not contain {"kind": "stem.state", "stem": "web", "to": "starting"}
    When I run "stems stamps --json"
    Then the JSON at "$.data.stamps" equals []
    When I run "stems down --all --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive

  Scenario: the dependants of a stem skipped for seed_degraded carry the same reason
    Given the fixture workspace "scripts-soft-fail"
    When I run "stems up front --detach --json"
    Then the exit code is 3
    And the JSON at "$.data.ready" equals ["db"]
    And the JSON at "$.data.skipped" equals ["web", "front"]
    And the JSON at "$.data.skipped_reasons" equals {"web": "seed_degraded", "front": "seed_degraded"}
    When I run "stems down --all --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive

  Scenario: a soft-failed build degrades the restart that ran it, not later starts
    Given the fixture workspace "scripts-soft-fail"
    When I run "stems up builder --detach --json"
    Then the command succeeds
    When I run "stems restart builder --build --json"
    Then the command succeeds
    And the JSON at "$.data.degraded[0]" equals {"stem": "builder", "script": "build", "exit": 5}
    When I run "stems status builder --json"
    Then the JSON at "$.data.stems[0].warnings[0].script" equals "build"
    # A successful build (BUILD_OK comes from the caller's environment) clears it.
    When I run "stems build builder --json" with env BUILD_OK=1
    Then the command succeeds
    When I run "stems status builder --json"
    Then the JSON at "$.data.stems[0].warnings" does not exist
    And the JSON at "$.data.stems[0].degraded" equals false
    When I run "stems restart builder --build --json"
    Then the command succeeds
    And the JSON at "$.data.degraded[0].script" equals "build"
    # A restart without a build does not keep the old build's warning.
    When I run "stems restart builder --json"
    Then the command succeeds
    And the JSON at "$.data.degraded" does not exist
    When I run "stems status builder --json"
    Then the JSON at "$.data.stems[0].warnings" does not exist
    When I run "stems down --all --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive

  Scenario: soft_fail is refused on scripts outside the start sequence
    Given the fixture workspace "scripts-soft-fail"
    And a local override setting stems.api.scripts.pre_stop={command: echo bye, soft_fail: true}
    When I run "stems validate --json"
    Then the exit code is 2
    And the JSON error has code "SCHEMA_INVALID" and path "stems.api.scripts.pre_stop.soft_fail"
    And the error message contains "soft_fail applies to setup, seed, pre_start, post_start, build and bootstrap"
