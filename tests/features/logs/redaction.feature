@FR-AI-3
Feature: Secret env values are redacted in show, env, status -v and logs
  Names are secret by the built-in patterns (`*TOKEN*`, `*PASSWORD*`, …),
  the workspace's or the stem's `secrets:` list, minus `!NAME` exceptions.
  `stems show`, `stems env` and `stems status -v` print `<redacted>` for
  them; in the stem's log stream every secret *value* of at least 6
  characters (from `env:` or `env_files`) is replaced by `<redacted:KEY>`
  before it is stored.

  Scenario: secrets never reach logs, show, env or status -v
    Given the fixture workspace "redaction"
    When I run "stems up --detach --json"
    Then the command succeeds
    And within 5s the stem log file "api/current.log" contains "greeting=hello-there"
    When I run "stems logs api --json"
    Then the command succeeds
    And the JSON at "$[?search(@.text, 'greeting=')].text" contains "token=<redacted:API_TOKEN>"
    And the JSON at "$[?search(@.text, 'greeting=')].text" contains "plain=<redacted:PLAIN>"
    And the JSON at "$[?search(@.text, 'greeting=')].text" contains "public=pk-public-1"
    And the JSON at "$[?search(@.text, 'greeting=')].text" contains "pin=1234"
    And the JSON at "$[?search(@.text, 'greeting=')].text" contains "file=<redacted:FILE_SECRET>"
    And stdout does not contain "s3cr3tvalue"
    And stdout does not contain "hunter22"
    And stdout does not contain "from-env-file-42"
    When I run "stems show api --json"
    Then the command succeeds
    And the JSON at "$.data.env.API_TOKEN" equals "<redacted>"
    And the JSON at "$.data.env.PLAIN" equals "<redacted>"
    And the JSON at "$.data.env.PUBLIC_KEY" equals "pk-public-1"
    And the JSON at "$.data.env.GREETING" equals "hello-there"
    When I run "stems show --json"
    Then the JSON at "$.data.stems.api.env.API_TOKEN" equals "<redacted>"
    When I run "stems status -v --json"
    Then the command succeeds
    And the JSON at "$.data.stems[?@.name=='api'].env.API_TOKEN" equals "<redacted>"
    And the JSON at "$.data.stems[?@.name=='api'].env.PLAIN" equals "<redacted>"
    And the JSON at "$.data.stems[?@.name=='api'].env.GREETING" equals "hello-there"
    When I run "stems env api --json"
    Then the command succeeds
    And the JSON at "$.data.env.API_TOKEN" equals "<redacted>"
    And the JSON at "$.data.env.PLAIN" equals "<redacted>"
    And stdout does not contain "s3cr3tvalue"
    When I run "stems down --json"
    Then the command succeeds
    And no process from the workspace's process groups is alive

  @error
  Scenario: --reveal is not allowed with --json
    Given the fixture workspace "redaction"
    When I run "stems show api --reveal --json"
    Then the exit code is 2
    And the JSON error has code "USAGE"
