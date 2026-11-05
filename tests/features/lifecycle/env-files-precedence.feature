@FR-ST-4
Feature: env_files beat env:, stems.local.yaml beats env_files
  Layers, later wins: workspace `env` → stem `env` → `env_files` (in
  order) → `stems.local.yaml` → `--pass-env`. An absent `required: false`
  file is skipped; a present one applies in its place in the order.

  Scenario: the env file wins over env:, an absent optional file is skipped
    Given the fixture workspace "env-files-demo"
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run "stems status -v --json"
    Then the JSON at "$.data.stems[0].state" equals "healthy"
    And the JSON at "$.data.stems[0].env.API_URL" equals "http://from-file"
    And the JSON at "$.data.stems[0].env.ONLY_FILE" equals "from file"
    And the JSON at "$.data.stems[0].env.SAME" equals "1"
    When I run "stems down --all --json"
    Then the command succeeds

  Scenario: a present optional file applies after the required one
    Given the fixture workspace "env-files-demo"
    And the file "env/api.local.env" is written with "API_URL=http://from-optional-file"
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run "stems status -v --json"
    Then the JSON at "$.data.stems[0].env.API_URL" equals "http://from-optional-file"
    When I run "stems down --all --json"
    Then the command succeeds

  Scenario: stems.local.yaml wins over the env file
    Given the fixture workspace "env-files-demo"
    And a local override setting stems.api.env.API_URL=http://from-local
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run "stems status -v --json"
    Then the JSON at "$.data.stems[0].env.API_URL" equals "http://from-local"
    And the JSON at "$.data.stems[0].env.ONLY_FILE" equals "from file"
    When I run "stems down --all --json"
    Then the command succeeds

  Scenario: a missing required env file fails the start before anything runs
    Given the fixture workspace "env-files-demo"
    And a local override setting stems.api.env_files=[env/missing.env]
    When I run "stems up --detach --json"
    Then the exit code is 2
    And the JSON at "$.errors[0].code" equals "ENV_FILE_NOT_FOUND"

  Scenario: an env file deleted while the daemon runs fails the next start
    Given the fixture workspace "env-files-demo"
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run the shell command "rm env/api.env"
    Then the exit code is 0
    When I run "stems restart api --json"
    Then the command fails
    And the JSON at "$.errors[0].code" equals "ENV_FILE_NOT_FOUND"
    And the JSON at "$.errors[0].details.stem" equals "api"
    When I run "stems down --all --json"
    Then the command succeeds
