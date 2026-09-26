@FR-WD-1
Feature: a rebuild action runs the build script, then restarts
  `action: rebuild` = the stem's `build` script (16), then a restart
  through the policy bypass. The build appends to build-marker.txt; its
  `script.finished` comes before the restart's `starting`. A failing build
  leaves the stem running (`watch.action_finished {ok: false}`).

  Scenario: build marker first, then the restart
    Given the fixture workspace "watch-shop"
    And the workspace has a private copy of the repos
    When I run "stems up rebuilt --detach --json"
    Then the command succeeds
    And within 15s the stem "rebuilt" is "healthy"
    When I run "stems status rebuilt --json"
    And I save the JSON at "$.data.stems[0].pid" as "pid"
    And I run the shell command "echo '# touched' >> ${tmp}/examples/repos/shop-api/app.py"
    Then within 5s the events stream contains {"kind": "watch.triggered", "stem": "rebuilt", "data": {"action": "rebuild"}}
    And within 20s the events stream contains {"kind": "watch.action_finished", "stem": "rebuilt", "data": {"action": "rebuild", "ok": true}}
    And the file "build-marker.txt" contains "built"
    And the events stream contains {"kind": "script.finished", "stem": "rebuilt", "data": {"script": "build", "exit": 0}} before {"kind": "stem.restarting", "stem": "rebuilt"}
    And the events stream contains {"kind": "script.finished", "stem": "rebuilt", "data": {"script": "build"}} before {"kind": "stem.state", "stem": "rebuilt", "to": "starting", "reason": "restarting (watch: app.py changed)"}
    And within 5s the stem "rebuilt" is "healthy"
    When I run "stems status rebuilt --json"
    Then the JSON at "$.data.stems[0].pid" does not equal ${var:pid}
    And the JSON at "$.data.stems[0].restarts" equals 0
    When I run "stems down --json"
    Then the command succeeds
