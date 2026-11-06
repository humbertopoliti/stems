@FR-ST-4
Feature: env_files `override: false` lets env: win, and starts warn ENV_SHADOWED
  By default an env file beats `env:`. An entry with `override: false` (or
  the workspace's `env_files_override: false`) keeps the `env:` value of a
  key `env:` sets; the file only adds the other keys. `ENV_SHADOWED` says
  which side won, and `up`, `start` and `restart` report it with their
  warnings (once per started stem) as `validate` does.

  Scenario: by default the file wins and up, start and restart warn ENV_SHADOWED
    Given the fixture workspace "env-files-demo"
    When I run "stems up --detach --json"
    Then the command succeeds
    And the JSON at "$.data.warnings[0].code" equals "ENV_SHADOWED"
    And the JSON at "$.data.warnings[0].details.stem" equals "api"
    And the JSON at "$.data.warnings[0].details.key" equals "API_URL"
    And the JSON at "$.data.warnings[0].details.winner" equals "file"
    And the JSON at "$.data.warnings[0].message" contains "shadows env: API_URL"
    And the JSON at "$.data.warnings[1]" does not exist
    And within 5s the daemon log contains "shadows env: API_URL"
    When I run "stems status -v --json"
    Then the JSON at "$.data.stems[0].env.API_URL" equals "http://from-file"
    When I run "stems restart api --json"
    Then the command succeeds
    And the JSON at "$.data.warnings[0].code" equals "ENV_SHADOWED"
    And the JSON at "$.data.warnings[1]" does not exist
    When I run "stems stop api --json"
    Then the command succeeds
    When I run "stems start api --human"
    Then the command succeeds
    And stdout contains "warning api: `API_URL` of `api` from env/api.env shadows env: API_URL"
    When I run "stems down --all --json"
    Then the command succeeds

  Scenario: override false keeps the env: value and the file adds the rest
    Given the fixture workspace "env-files-demo"
    And the local override file is extended with:
      """
      stems:
        api:
          env_files:
            - { path: env/api.env, override: false }
      """
    When I run "stems validate --json"
    Then the exit code is 0
    And the JSON at "$.data.warnings[0].code" equals "ENV_SHADOWED"
    And the JSON at "$.data.warnings[0].details.winner" equals "env"
    And the JSON at "$.data.warnings[0].message" equals "env: API_URL of `api` keeps its value; env/api.env's API_URL is ignored (override: false)"
    When I run "stems up --detach --json"
    Then the command succeeds
    And the JSON at "$.data.warnings[0]" does not exist
    When I run "stems status -v --json"
    Then the JSON at "$.data.stems[0].env.API_URL" equals "http://from-env"
    And the JSON at "$.data.stems[0].env.ONLY_FILE" equals "from file"
    When I run "stems env api --json"
    Then the JSON at "$.data.env.API_URL" equals "http://from-env"
    When I run "stems down --all --json"
    Then the command succeeds

  Scenario: the workspace default applies to every entry, an entry's own override wins
    Given the fixture workspace "env-files-demo"
    And the local override file is extended with:
      """
      env_files_override: false
      """
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run "stems status -v --json"
    Then the JSON at "$.data.stems[0].env.API_URL" equals "http://from-env"
    And the JSON at "$.data.stems[0].env.ONLY_FILE" equals "from file"
    When I run "stems down --all --json"
    Then the command succeeds
    Given the local override file is extended with:
      """
      stems:
        api:
          env_files:
            - { path: env/api.env, override: true }
      """
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run "stems status -v --json"
    Then the JSON at "$.data.stems[0].env.API_URL" equals "http://from-file"
    When I run "stems down --all --json"
    Then the command succeeds
