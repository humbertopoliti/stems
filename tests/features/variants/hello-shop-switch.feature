@docker @FR-ST-8 @FR-ST-3 @slow
Feature: hello-shop's shop-api switches between a process and a container
  examples/workspaces/hello-shop declares a `docker` variant for shop-api:
  the same codebase built from its Dockerfile, published on shop-api's host
  port → container port 8080, reaching postgres through
  `host.docker.internal`, with the overlay's config/local.ini mounted.
  The harness remaps shop-api's host port through stems.local.yaml; the
  replacing `ports` entry keeps the variant's `container_port: 8080` by
  port name (crates/stems-config README, merge rules). Needs Docker and
  internet (httpbin): `make e2e-docker`.

  Scenario: switch shop-api to docker and back while the rest keeps running
    Given the "hello-shop" workspace
    # shop-api's overlay (config/local.ini) is written into its codebase.
    And the workspace has a private copy of the repos
    When I run "stems up --detach --json"
    Then the command succeeds
    When I run "stems status --json"
    Then the JSON at "$.data.stems[?@.name=='shop-api'].type" equals "process"
    And the JSON at "$.data.stems[?@.name=='shop-api'].variant" equals "local"
    When I save the JSON at "$.data.stems[?@.name=='shop-web'].pid" as "web_pid"
    When I run "stems switch shop-api docker --json"
    Then the command succeeds
    And the JSON at "$.data.type" equals "docker"
    And the JSON at "$.data.applied.applied[?@.stem=='shop-api'].result" equals "restarted"
    And the container "hello-shop-shop-api" is running with label "stems.stem=shop-api"
    When I run "stems status --json"
    Then the JSON at "$.data.stems[?@.name=='shop-api'].type" equals "docker"
    And the JSON at "$.data.stems[?@.name=='shop-api'].variant" equals "docker"
    And the JSON at "$.data.stems[?@.name=='shop-api'].state" equals "healthy"
    And the JSON at "$.data.stems[?@.name=='shop-api'].pid" equals null
    And the JSON at "$.data.stems[?@.name=='shop-web'].pid" equals ${var:web_pid}
    # The container listens on 8080 (kept through the harness's port remap).
    When I run the shell command "docker port hello-shop-shop-api 8080/tcp"
    Then the exit code is 0
    And stdout contains ":${port:18080}"
    When I run the shell command "curl -fsS http://127.0.0.1:${port:18080}/healthz"
    Then the exit code is 0
    # No psql in the image: /products serves its in-memory list.
    When I run the shell command "curl -fsS http://127.0.0.1:${port:18080}/products"
    Then the exit code is 0
    And stdout contains "Espresso cup"
    When I run "stems switch shop-api local --json"
    Then the command succeeds
    And the JSON at "$.data.type" equals "process"
    When I run the shell command "docker ps -a --format '{{.Names}}' --filter label=stems.workspace=hello-shop"
    Then the exit code is 0
    And stdout contains "hello-shop-postgres"
    And stdout does not contain "hello-shop-shop-api"
    When I run "stems status --json"
    Then the JSON at "$.data.stems[?@.name=='shop-api'].type" equals "process"
    And the JSON at "$.data.stems[?@.name=='shop-api'].state" equals "healthy"
    And the JSON at "$.data.stems[?@.name=='shop-web'].pid" equals ${var:web_pid}
    # Back on the host: /products reads the seeded rows through SHOP_PSQL.
    When I run the shell command "curl -fsS http://127.0.0.1:${port:18080}/products"
    Then the exit code is 0
    And stdout contains "Widget"
    When I run "stems down --all --volumes --yes --json"
    Then the command succeeds
    And no container with label stems.workspace=hello-shop exists
    And no process from the workspace's process groups is alive
    When I run the shell command "docker image ls -q stems/hello-shop/shop-api | xargs docker image rm -f"
