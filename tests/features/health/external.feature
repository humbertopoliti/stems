@FR-ST-3 @FR-HS-2
Feature: external stems are monitored by their probe
  An external stem is never started or stopped, but with a health check its
  state follows the probe: `unknown` until the first result, `healthy` when
  it passes, `unhealthy` after `retries` failures (connection refused), and
  `unknown` while the probe cannot run at all (the host does not resolve).
  A `condition: healthy` edge to it waits for `healthy`, bounded by its
  `health.start_timeout` (HEALTH_TIMEOUT). `down` ends the monitoring.
  The harness serves the external with `python3 -m http.server`.

  Scenario: healthy while served, unhealthy once the server stops
    Given the fixture workspace "health-external"
    And a stray process "exec python3 -m http.server ${port:18731} --bind 127.0.0.1" is running in a new process group
    Then within 10s port ${port:18731} is listening
    When I run "stems up --detach --json"
    Then the command succeeds
    And the JSON at "$.data.ready" equals ["hosted", "api"]
    And the events stream contains {"kind": "stem.state", "stem": "hosted", "to": "healthy"} before {"kind": "stem.state", "stem": "api", "to": "starting"}
    When I run "stems status hosted --json"
    Then the JSON at "$.data.stems[0].state" equals "healthy"
    And the JSON at "$.data.stems[0].pid" equals null
    And the JSON at "$.data.stems[0].health.type" equals "http"
    When the stray processes are stopped
    Then within 3s the stem "hosted" is "unhealthy"
    When I run "stems status --json"
    Then the JSON at "$.data.stems[?@.name=='hosted'].reason" contains "connection refused"
    And the JSON at "$.data.stems[?@.name=='api'].degraded" equals true
    And the JSON at "$.data.stems[?@.name=='api'].reason" equals "dependency hosted unhealthy"
    And within 5s the events stream contains {"kind": "stem.health", "stem": "hosted", "from": "healthy", "to": "unhealthy"}
    When I run "stems down --json"
    Then the command succeeds
    And the JSON at "$.data.stopped" equals ["api"]
    # monitoring ended (hosted is `stopped`), so nothing runs: the daemon `up`
    # started exits too
    And the JSON at "$.data.daemon_stopping" equals true
    And no process from the workspace's process groups is alive

  Scenario: an unresolvable host is unknown, not unhealthy
    Given the fixture workspace "health-external"
    And a local override setting stems.hosted.health.url=http://nonexistent.invalid:1/
    When I run "stems up hosted --detach --json"
    Then the command succeeds
    And within 3s the JSON at "$.data.stems[0].results[-1:].outcome" of "stems health hosted --json" equals "unknown"
    When I run "stems status hosted --json"
    Then the JSON at "$.data.stems[0].state" equals "unknown"
    And the JSON at "$.data.stems[0].glyph" equals "unknown"
    And the JSON at "$.data.stems[0].health.last.detail" contains "cannot resolve host"
    When I run "stems down --json"
    Then the command succeeds

  @error
  Scenario: a dependant of an unreachable external is skipped after its start_timeout
    Given the fixture workspace "health-external"
    And a local override setting stems.hosted.health.start_timeout=1s
    When I run "stems up --detach --json"
    Then the exit code is 1
    And the JSON at "$.data.failed[0].stem" equals "hosted"
    And the JSON at "$.data.failed[0].error.code" equals "HEALTH_TIMEOUT"
    And the JSON at "$.data.skipped" equals ["api"]
    When I run "stems status --json"
    Then the JSON at "$.data.stems[?@.name=='api'].state" equals "stopped"
    When I run "stems down --all --json"
    Then the command succeeds
