@FR-WS-3 @error
Feature: git failures are reported with git's own words
  An unreachable repository is `GIT_CLONE_FAILED` (exit 1) with the last
  lines of git's stderr in `details.tail`; during `up` the stem fails and
  the stems that do not need it still start. A `git:` value that is not a
  URL is a SCHEMA_INVALID config error.

  Scenario: repos sync of an unreachable URL
    Given the fixture workspace "git-codebase"
    When I run "stems repos sync --json"
    Then the exit code is 1
    And the JSON error has code "GIT_CLONE_FAILED"
    And the JSON at "$.errors[0].details.url" equals "file:///nonexistent/repo.git"
    And the JSON at "$.errors[0].details.ref" equals "main"
    And the JSON at "$.errors[0].details.tail[0]" exists
    And the JSON at "$.data.repos[0].action" equals "failed"
    And the file ".stems/repos/shop-api" does not exist

  Scenario: up fails the stem and starts the others
    Given the fixture workspace "git-codebase"
    When I run "stems up --detach --no-fail-fast --json"
    Then the exit code is 3
    And the JSON at "$.data.ready" contains "hosted"
    And the JSON at "$.data.failed[0].stem" equals "shop-api"
    And the JSON at "$.data.failed[0].error.code" equals "GIT_CLONE_FAILED"
    And the JSON at "$.data.failed[0].error.details.tail[0]" exists
    And within 5s the events stream contains {"kind": "repo.failed", "stem": "shop-api"}
    When I run "stems status --json"
    Then the JSON at "$.data.stems[?@.name=='shop-api'].state" equals "failed"
    When I run "stems down --all --json"
    Then the command succeeds

  Scenario: a git value that is not a URL is invalid config
    Given the fixture workspace "git-codebase"
    And a local override setting stems.shop-api.codebase.git=not-a-url
    When I run "stems validate --json"
    Then the exit code is 2
    And the JSON error has code "SCHEMA_INVALID" and path "stems.shop-api.codebase.git"
