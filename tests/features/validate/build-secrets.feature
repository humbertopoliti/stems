@FR-ST-2
Feature: validate rejects malformed build secrets and allow_read entries
  `build.secrets` entries set exactly one of `file`/`env`; `build.allow_read`
  entries are exact absolute paths: no glob characters, no symlinks (list the
  symlink's target instead). Docker is not needed: the fixture is only
  validated.

  @error
  Scenario: a secret with both file and env is rejected
    Given the fixture workspace "docker-build-secrets"
    And a local override setting stems.api.build.secrets=[{id: tok, file: secrets/tok.txt, env: TOK}]
    When I run "stems validate --json"
    Then the exit code is 2
    And the JSON error has code "SCHEMA_INVALID" and path "stems.api.build.secrets[0]"

  @error
  Scenario: a glob in allow_read is rejected
    Given the fixture workspace "docker-build-secrets"
    And a local override setting stems.api.build.allow_read=["${ws}/infra/*"]
    When I run "stems validate --json"
    Then the exit code is 2
    And the JSON error has code "SCHEMA_INVALID" and path "stems.api.build.allow_read[0]"
    And the error message contains "glob"

  @error
  Scenario: a symlink in allow_read is rejected
    Given the fixture workspace "docker-build-secrets"
    And a local override setting stems.api.build.allow_read=["${ws}/infra-link"]
    When I run the shell command "ln -s ${ws}/infra ${ws}/infra-link"
    And I run "stems validate --json"
    Then the exit code is 2
    And the JSON error has code "SCHEMA_INVALID" and path "stems.api.build.allow_read[0]"
    And the error message contains "symlink"
