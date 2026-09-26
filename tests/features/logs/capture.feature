@FR-LG-1
Feature: Stem output is captured, timestamped and persisted
  Every line a stem prints becomes a log record (daemon receive time,
  stream tag, detected level) kept in the daemon's in-memory ring and
  appended to `$STEMS_HOME/<ws>/logs/<stem>/current.log` whether or not a
  client is attached. shop-api writes every log line, errors included, to
  stdout (`log()` in app.py), so its error lines are `stream: out` with
  `level: error`.

  Scenario: 50 error lines are queryable by level and on disk
    Given the "minimal" workspace is up
    When the chaos endpoint "logs?n=50&level=error" is called on "echo-svc"
    Then the chaos response status is 200
    When I run "stems logs echo-svc --json --level error"
    Then the command succeeds
    And the JSON at "$[49].level" equals "error"
    And the JSON at "$[49].text" equals "ERROR chaos log line 49"
    And the JSON at "$[?@.level != 'error']" does not exist
    And the JSON at "$[?@.stream != 'out']" does not exist
    And the JSON at "$[0].stem" equals "echo-svc"
    And the JSON at "$[0].tag" equals null
    And the JSON at "$[*].ts" is in ascending order
    And within 2s the stem log file "echo-svc/current.log" contains "ERROR chaos log line 0"
    And within 2s the stem log file "echo-svc/current.log" contains "ERROR chaos log line 49"
    When I run "stems down --json"
    Then the command succeeds

  @error
  Scenario: an unknown stem is a usage error
    Given the "minimal" workspace is up
    When I run "stems logs nope --json"
    Then the exit code is 2
    And the JSON error has code "UNKNOWN_STEM"
    When I run "stems logs --json --level loud"
    Then the exit code is 2
    And the JSON error has code "USAGE"
    When I run "stems down --json"
    Then the command succeeds
