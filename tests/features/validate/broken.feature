@FR-WS-6 @error
Feature: The broken example workspaces fail validation as documented
  Every examples/workspaces/broken/<name>/ directory holds an EXPECTED.json
  naming the first error `stems validate --json` reports (errors are sorted
  by file, line, column). The Examples table is generated from those files
  and kept in sync by scripts/tests/test_broken_examples_in_sync.py; after
  adding a broken workspace run
  `python3 scripts/tests/test_broken_examples_in_sync.py --write`.
  (Needs `stems validate`, deliverable 07; the semantics are proven today by
  crates/stems-core/tests/broken_examples.rs.)

  Scenario Outline: broken/<name> is rejected with <code>
    Given the broken workspace "<name>"
    When I run "stems validate --json"
    Then the exit code is <exit>
    And the JSON at "$.ok" equals false
    And the JSON error has code "<code>" and path "<path>"
    And the error message contains "<message_contains>"

    Examples:
      | name                | code                | path                            | exit | message_contains                    |
      | bad-schema          | SCHEMA_INVALID      | stems.shop-api.type             | 2    | rocket                              |
      | codebase-not-found  | CODEBASE_NOT_FOUND  | stems.shop-api.codebase         | 2    | does-not-exist                      |
      | cycle               | CYCLE               | stems.shop-api.depends_on       | 2    | shop-api -> shop-worker -> shop-api |
      | missing-script      | SCRIPT_NOT_FOUND    | stems.shop-api.scripts.start    | 2    | scripts/nope.sh                     |
      | overlay-conflict    | OVERLAY_CONFLICT    | stems.local-svc.overlays        | 2    | config/local.ini                    |
      | port-conflict       | PORT_CONFLICT       | stems.postgres-replica.ports    | 2    | 5432                                |
      | seeded-without-seed | SEEDED_WITHOUT_SEED | stems.shop-worker.depends_on    | 2    | postgres                            |
      | tool-version        | TOOL_VERSION        | requires.node                   | 2    | >=99                                |
      | unknown-dependency  | UNKNOWN_DEPENDENCY  | stems.shop-api.depends_on       | 2    | postgres                            |
      | unresolved-variable | UNRESOLVED_VARIABLE | stems.shop-api.env.DATABASE_URL | 2    | var.nope                            |
