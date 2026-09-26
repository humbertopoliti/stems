@FR-LG-5
Feature: Exporting a log bundle
  `stems logs --export -o <file>` writes a .tar.gz with `status.json`,
  `events.ndjson`, the resolved config (`config.json`, secrets redacted)
  and every stem's log files.

  Scenario: the bundle holds status, events, redacted config and logs
    Given the "minimal" workspace with a local override setting stems.echo-svc.env.API_TOKEN=hunter2
    When I run "stems up --detach --json"
    Then the command succeeds
    When the chaos endpoint "logs?n=3&level=error" is called on "echo-svc"
    And I run "stems logs --export -o out/bundle.tar.gz --json"
    Then the command succeeds
    And the JSON at "$.data.entries" contains "status.json"
    And the file "out/bundle.tar.gz" exists
    And the archive "out/bundle.tar.gz" contains "status.json"
    And the archive "out/bundle.tar.gz" contains "events.ndjson"
    And the archive "out/bundle.tar.gz" contains "config.json"
    And the archive "out/bundle.tar.gz" contains "logs/echo-svc/"
    And the archive "out/bundle.tar.gz" entry "logs/echo-svc/current.log" contains "ERROR chaos log line 2"
    And the archive "out/bundle.tar.gz" entry "events.ndjson" contains "stem.state"
    And the archive "out/bundle.tar.gz" entry "status.json" contains "echo-svc"
    And the archive "out/bundle.tar.gz" entry "config.json" contains "<redacted>"
    And the archive "out/bundle.tar.gz" entry "config.json" does not contain "hunter2"
    When I run "stems down --json"
    Then the command succeeds
