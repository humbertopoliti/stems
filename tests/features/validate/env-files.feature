@FR-ST-4
Feature: env_files are checked by validate and doctor
  A required `env_files` entry that does not exist is `ENV_FILE_NOT_FOUND`
  (exit 2) at `validate`; a `{path, required: false}` entry may be absent.
  An env file that sets a key `env:` (or an earlier file) sets to a
  different value wins, and `validate` warns `ENV_SHADOWED` (exit 0).
  `doctor` reports the same as `env.<stem>`.

  Scenario: a shadowed key is a warning, an equal value is not
    Given the fixture workspace "env-files-demo"
    When I run "stems validate --json"
    Then the exit code is 0
    And the JSON at "$.data.ok" equals true
    And the JSON at "$.data.warnings[0].code" equals "ENV_SHADOWED"
    And the JSON at "$.data.warnings[0].path" equals "stems.api.env_files[0]"
    And the JSON at "$.data.warnings[0].details.key" equals "API_URL"
    And the JSON at "$.data.warnings[0].details.shadows" equals "env"
    And the JSON at "$.data.warnings[0].details.file" equals "env/api.env"
    And the JSON at "$.data.warnings[1]" does not exist
    When I run "stems doctor --json"
    Then the JSON at "$.data.checks[?@.id == 'env.api'].status" equals "warn"
    And the JSON at "$.data.checks[?@.id == 'env.api'].code" equals "ENV_SHADOWED"

  Scenario: equal values and an absent optional file are fine
    Given the fixture workspace "env-files-demo"
    And a local override setting stems.api.env.API_URL=http://from-file
    When I run "stems validate --json"
    Then the exit code is 0
    And the JSON at "$.data.warnings[0]" does not exist
    When I run "stems doctor --json"
    Then the JSON at "$.data.checks[?@.id == 'env.api'].status" equals "ok"
    And the JSON at "$.data.checks[?@.id == 'env.api'].message" contains "env/api.local.env optional, absent"

  Scenario: a missing required env file fails validate and doctor
    Given the fixture workspace "env-files-demo"
    And a local override setting stems.api.env_files=[env/api.env, env/missing.env]
    When I run "stems validate --json"
    Then the exit code is 2
    And the JSON at "$.errors[0].code" equals "ENV_FILE_NOT_FOUND"
    And the JSON at "$.errors[0].path" equals "stems.api.env_files[1]"
    And the JSON at "$.errors[0].details.stem" equals "api"
    When I run "stems doctor --json"
    Then the JSON at "$.data.checks[?@.id == 'env.api'].status" equals "fail"
    And the JSON at "$.data.checks[?@.id == 'env.api'].code" equals "ENV_FILE_NOT_FOUND"
