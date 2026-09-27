@FR-HS-1 @FR-HS-2
Feature: HTTP health probes drive healthy and unhealthy
  A stem's health check runs every `interval` for as long as the stem runs.
  `retries` failed probes in a row make a healthy stem `unhealthy` (glyph
  failed, reason = the last probe error); one passing probe makes it
  `healthy` again. Each of those transitions is one `stem.health` event
  (probes themselves are not streamed; `stems health` shows them). Here the
  minimal workspace's echo-svc gets an http probe on /healthz every 200 ms
  with retries 2 (a health check that changes `type` replaces the base one
  whole, so the override repeats `interval`/`timeout`), and the chaos
  endpoint makes /healthz answer 503 for 2 s.

  Scenario: an http probe follows the service's /healthz
    Given the "minimal" workspace
    And a local override setting stems.echo-svc.health.type=http
    And a local override setting stems.echo-svc.health.url=http://127.0.0.1:${port:18090}/healthz
    And a local override setting stems.echo-svc.health.retries=2
    And a local override setting stems.echo-svc.health.interval=200ms
    And a local override setting stems.echo-svc.health.timeout=150ms
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run "stems status --json"
    Then the JSON at "$.data.stems[0].state" equals "healthy"
    And the JSON at "$.data.stems[0].glyph" equals "healthy"
    And the JSON at "$.data.stems[0].reason" equals null
    And the JSON at "$.data.stems[0].degraded" equals false
    And the JSON at "$.data.stems[0].health.type" equals "http"
    And the JSON at "$.data.stems[0].health.last.ok" equals true
    And the JSON at "$.data.stems[0].health.last.detail" equals "HTTP 200"
    When the chaos endpoint "unhealthy?for=2s" is called on "echo-svc"
    Then the chaos response status is 200
    And within 1.5s the stem "echo-svc" is "unhealthy"
    When I run "stems status --json"
    Then the JSON at "$.data.stems[0].glyph" equals "failed"
    And the JSON at "$.data.stems[0].reason" contains "HTTP 503"
    And the JSON at "$.data.summary.unhealthy" equals 1
    And the JSON at "$.data.summary.failed" equals 0
    And within 4s the stem "echo-svc" is "healthy"
    And there are exactly 2 events matching {"kind": "stem.health", "stem": "echo-svc"}
    And the events stream contains {"kind": "stem.health", "stem": "echo-svc", "from": "healthy", "to": "unhealthy"} before {"kind": "stem.health", "stem": "echo-svc", "from": "unhealthy", "to": "healthy"}
    And within 1s the events stream contains {"kind": "stem.health", "stem": "echo-svc", "to": "unhealthy", "data": {"probe": "http", "outcome": "fail"}}
    When I run "stems status --json"
    Then the JSON at "$.data.stems[0].reason" equals null
    When I run "stems health echo-svc --last 50 --json"
    Then the command succeeds
    And the JSON at "$.data.stems[0].type" equals "http"
    And the JSON at "$.data.stems[0].results" contains {"ok": false, "outcome": "fail", "detail": "HTTP 503 (want 2xx)"}
    And the JSON at "$.data.stems[0].results" contains {"ok": true, "detail": "HTTP 200"}
    When I run "stems down --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive
