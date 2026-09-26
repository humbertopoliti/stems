@docker @FR-LC-1 @FR-LC-2 @slow
Feature: hello-shop from zero to ready (phase 2 exit criterion)
  On a clean STEMS_HOME, `stems up` runs the whole sequence for hello-shop:
  workspace bootstrap, then per stem setup (stamped) → start → wait →
  seed. postgres is seeded before shop-worker (`condition: seeded`) starts,
  shop-api serves the seeded products, and `down --all` removes every
  process and container (and runs the workspace teardown).

  Needs Docker (postgres is a docker stem, redis a compose stem), `curl`,
  and a `psql` client on the host: shop-api reads `/products` through
  `psql $DATABASE_URL` (without psql it falls back to built-in products).
  Runs only under `make e2e-docker`.

  Scenario: up on a clean home, seeded rows served, down --all clean
    Given the "hello-shop" workspace
    When I run "stems up --detach --json"
    Then the command succeeds
    And the events stream contains {"kind": "script.finished", "data": {"script": "bootstrap", "exit": 0}} before {"kind": "stem.state", "stem": "postgres", "to": "starting"}
    And the events stream contains {"kind": "stem.state", "stem": "postgres", "to": "healthy"} before {"kind": "script.started", "stem": "postgres", "data": {"script": "seed"}}
    And the events stream contains {"kind": "script.finished", "stem": "postgres", "data": {"script": "seed", "exit": 0}} before {"kind": "stem.state", "stem": "shop-worker", "to": "starting"}
    And the events stream contains {"kind": "script.finished", "stem": "shop-api", "data": {"script": "setup", "exit": 0}} before {"kind": "stem.state", "stem": "shop-api", "to": "starting"}
    When I run "stems status --json"
    Then the JSON at "$.data.stems[?@.name=='postgres'].state" equals "healthy"
    And the JSON at "$.data.stems[?@.name=='postgres'].seeded" equals true
    And the JSON at "$.data.stems[?@.name=='shop-api'].state" equals "healthy"
    And the JSON at "$.data.stems[?@.name=='shop-worker'].state" equals "healthy"
    When I run the shell command "docker exec hello-shop-postgres psql -U shop -d shop -Atc 'SELECT count(*) > 0 FROM products'"
    Then the exit code is 0
    And stdout contains "t"
    When I run the shell command "curl -fsS http://127.0.0.1:${port:18080}/products"
    Then the exit code is 0
    And stdout contains "Widget"
    And stdout contains "Gadget"
    When I run "stems stamps --json"
    Then the JSON at "$.data.stamps[?@.stem=='postgres'].script" equals "seed"
    When I run "stems down --all --json"
    Then the command succeeds
    And no container with label stems.workspace=hello-shop exists
    And no process from the workspace's process groups is alive
    And the state file contains no stems
