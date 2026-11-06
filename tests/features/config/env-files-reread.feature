@FR-ST-4
Feature: env files are re-read on every env computation
  A key removed from an env file is gone from the next `stems env`, `stems
  exec` and start: nothing caches the parsed file. A running stem keeps the
  environment it was started with until it is restarted; `stems env` and
  `stems status` report the drift as `ENV_STALE`.

  Scenario: a key removed from an env file while the stem runs
    Given the fixture workspace "env-files-demo"
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run "stems env api --json"
    Then the JSON at "$.data.env.ONLY_FILE" equals "from file"
    And the JSON at "$.data.warnings[0]" does not exist
    When I run the shell command "printf 'API_URL=http://from-file\nSAME=1\n' > env/api.env"
    Then the exit code is 0
    When I run "stems env api --json"
    Then the JSON at "$.data.env.ONLY_FILE" does not exist
    And the JSON at "$.data.env.API_URL" equals "http://from-file"
    And the JSON at "$.data.warnings[0].code" equals "ENV_STALE"
    And the JSON at "$.data.warnings[0].details.keys" equals ["ONLY_FILE"]
    When I run "stems env api"
    Then stderr contains "warning: ENV_STALE: `api` runs with an environment that differs from its config now: ONLY_FILE"
    When I run "stems exec api -- sh -c 'echo only=[$ONLY_FILE]'"
    Then stdout contains "only=[]"
    When I run "stems status --json"
    Then the JSON at "$.data.stems[0].env_stale" equals ["ONLY_FILE"]
    When I run "stems status --human"
    Then stdout contains "warning api: ENV_STALE: env changed since it started (ONLY_FILE)"
    When I run "stems status -v --json"
    Then the JSON at "$.data.stems[0].env.ONLY_FILE" equals "from file"
    When I run "stems restart api --json"
    Then the command succeeds
    When I run "stems status -v --json"
    Then the JSON at "$.data.stems[0].env.ONLY_FILE" does not exist
    And the JSON at "$.data.stems[0].env_stale" does not exist
    When I run "stems env api --json"
    Then the JSON at "$.data.warnings[0]" does not exist
    When I run "stems down --all --json"
    Then the command succeeds

  Scenario: a dependency restarting is not drift and keeps the live values
    Only the config layers count as drift: a dependency's new output, or
    no output while it restarts, is neither ENV_STALE nor a placeholder.
    Fixture: tests/fixtures/workspaces/env-stale-deps (`auth` takes ~2 s
    to become healthy; `api` has a soft edge to it, so no cascade).
    Given the fixture workspace "env-stale-deps"
    And the file "stamp.txt" is written with "t1"
    When I run "stems up --detach --json auth"
    Then the command succeeds
    When I run "stems up --detach --json api"
    Then the command succeeds
    When I run "stems env api --json"
    Then the JSON at "$.data.env.AUTH_STAMP" equals "t1"
    And the JSON at "$.data.env.STEMS_AUTH_OUTPUT_STAMP" equals "t1"
    And the JSON at "$.data.warnings[0]" does not exist
    Given the file "stamp.txt" is written with "t2"
    When I run "stems restart auth --json" in the background
    Then within 10s the stem "auth" is "starting"
    When I run "stems env api --json"
    Then the JSON at "$.data.env.AUTH_STAMP" equals "t1"
    And the JSON at "$.data.env.STEMS_AUTH_OUTPUT_STAMP" equals "t1"
    And the JSON at "$.data.unresolved[0]" does not exist
    And the JSON at "$.data.warnings[0]" does not exist
    When I run "stems status --json api"
    Then the JSON at "$.data.stems[0].env_stale" does not exist
    Then within 15s the stem "auth" is "healthy"
    When the background command is stopped
    When I run "stems status --json api"
    Then the JSON at "$.data.stems[0].env_stale" does not exist
    When I run "stems env api --json"
    Then the JSON at "$.data.env.AUTH_STAMP" equals "t2"
    And the JSON at "$.data.warnings[0]" does not exist
    When I run the shell command "printf 'FROM_FILE=2\n' > api.env"
    Then the exit code is 0
    When I run "stems status --json api"
    Then the JSON at "$.data.stems[0].env_stale" equals ["FROM_FILE"]
    When I run "stems down --all --json"
    Then the command succeeds
