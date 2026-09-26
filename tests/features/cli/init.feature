@FR-WS-5
Feature: stems init scaffolds an integration repo
  `stems init` writes stems.yaml (with a yaml-language-server schema
  reference), .gitignore, scripts/, env/ and overlays/. `--from` copies an
  example workspace with its codebases rewritten to ./repos/<name>
  placeholders. It never overwrites an existing stems.yaml.

  Scenario: init --from minimal in an empty directory creates a valid workspace
    Given an empty directory
    When I run "stems init --from minimal --name demo --json"
    Then the command succeeds
    And the JSON at "$.data.name" equals "demo"
    And the JSON at "$.data.created" contains "stems.yaml"
    And the file "stems.yaml" exists
    And the file ".gitignore" exists
    And the file "scripts" exists
    And the file "env" exists
    And the file "overlays" exists
    And the file "repos/shop-api" exists
    When I run "stems validate --skip-requires --json"
    Then the command succeeds
    And the JSON at "$.data.start_order" equals [["echo-svc"]]
    When I run "stems show --json"
    Then the command succeeds
    And the JSON at "$.data.name" equals "demo"

  Scenario: init --from hello-shop validates without Docker or tool checks
    Given an empty directory
    When I run "stems init --from hello-shop --json"
    Then the command succeeds
    And the file "scripts/postgres/seed.sh" exists
    And the file "compose/redis.yml" exists
    When I run "stems validate --skip-requires --json"
    Then the command succeeds

  Scenario: init without --from writes an empty workspace that validates
    Given an empty directory
    When I run "stems init --json"
    Then the command succeeds
    When I run "stems validate --json"
    Then the command succeeds
    And the JSON at "$.data.start_order" equals []

  @error
  Scenario: init refuses to overwrite an existing stems.yaml
    Given an empty directory
    When I run "stems init --from minimal --json"
    Then the command succeeds
    When I run "stems init --from minimal --json"
    Then the exit code is 1
    And the JSON error has code "ALREADY_INITIALISED"

  @error
  Scenario: init inside an existing workspace is refused
    Given the "minimal" workspace
    When I run "stems init --json"
    Then the exit code is 1
    And the JSON at "$.ok" equals false
    And the JSON error has code "ALREADY_INITIALISED"
