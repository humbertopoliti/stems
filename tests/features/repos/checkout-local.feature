@FR-WS-3
Feature: repos checkout also moves a local-path codebase that is a git work tree
  `stems repos checkout <stem> <ref>` on a codebase that is a plain path
  switches a local branch of that name (attached), else checks out a remote
  branch, tag or commit detached (`detached: true`). It refuses uncommitted
  changes (`REPO_DIRTY`) unless `--force`, and a path that is not a git
  repository (`REPO_NOT_GIT`).

  Background:
    Given the fixture workspace "git-codebase"
    And a local override setting stems.shop-api.codebase=${outside}/web

  Scenario: a local branch is checked out attached, a tag detached
    When I run the shell command "mkdir -p ${outside}/web && cd ${outside}/web && echo app > app.py && git init -q -b main && git add -A && git -c user.name=t -c user.email=t@example.com commit -q -m one && git tag v1 && git checkout -q -b feature && echo 2 > F && git add F && git -c user.name=t -c user.email=t@example.com commit -q -m two && git checkout -q main"
    Then the command succeeds
    When I run "stems repos checkout shop-api feature --json"
    Then the command succeeds
    And the JSON at "$.data.ref" equals "feature"
    And the JSON at "$.data.detached" does not exist
    And the file "${outside}/web/F" contains "2"
    When I run "stems repos status --json"
    Then the JSON at "$.data.repos[0].source" equals "local"
    And the JSON at "$.data.repos[0].branch" equals "feature"
    When I run "stems repos checkout shop-api v1 --json"
    Then the command succeeds
    And the JSON at "$.data.detached" equals true
    And the JSON at "$.data.message" contains "(detached)"
    And the file "${outside}/web/F" does not exist
    When I run "stems repos status --json"
    Then the JSON at "$.data.repos[0].branch" equals null
    And the JSON at "$.data.repos[0].checked_out" equals "v1"

  Scenario: a dirty tree is refused; --force carries the change over
    When I run the shell command "mkdir -p ${outside}/web && cd ${outside}/web && echo app > app.py && git init -q -b main && git add -A && git -c user.name=t -c user.email=t@example.com commit -q -m one && git tag v1 && echo 2 > F && git add F && git -c user.name=t -c user.email=t@example.com commit -q -m two"
    Then the command succeeds
    Given the file "${outside}/web/app.py" is written with "# my local work"
    When I run "stems repos checkout shop-api v1 --json"
    Then the exit code is 1
    And the JSON error has code "REPO_DIRTY"
    And the JSON at "$.errors[0].hint" contains "--force"
    When I run "stems repos checkout shop-api v1 --force --json"
    Then the command succeeds
    And the JSON at "$.data.ref" equals "v1"
    And the file "${outside}/web/app.py" contains "# my local work"

  Scenario: with no remote (offline) an existing local branch is switched to
    When I run the shell command "mkdir -p ${outside}/web && cd ${outside}/web && echo app > app.py && git init -q -b main && git add -A && git -c user.name=t -c user.email=t@example.com commit -q -m one && git checkout -q -b feature && echo 2 > F && git add F && git -c user.name=t -c user.email=t@example.com commit -q -m two && git checkout -q main"
    Then the command succeeds
    When I run the shell command "cd ${outside}/web && git remote add upstream file:///nonexistent/offline.git"
    Then the command succeeds
    When I run "stems repos checkout shop-api feature --json"
    Then the command succeeds
    And the JSON at "$.data.ref" equals "feature"
    And the JSON at "$.data.message" contains "warning"
    And the file "${outside}/web/F" contains "2"

  Scenario: a detached HEAD with local-only commits is refused, then --force
    When I run the shell command "mkdir -p ${outside}/web && cd ${outside}/web && echo app > app.py && git init -q -b main && git add -A && git -c user.name=t -c user.email=t@example.com commit -q -m one && git tag v1 && git checkout -q --detach && echo 2 > F && git add F && git -c user.name=t -c user.email=t@example.com commit -q -m lost"
    Then the command succeeds
    When I run "stems repos checkout shop-api main --json"
    Then the exit code is 1
    And the JSON error has code "REPO_DIRTY"
    And the JSON at "$.errors[0].hint" contains "--force"
    And the file "${outside}/web/F" contains "2"
    When I run "stems repos checkout shop-api main --force --json"
    Then the command succeeds
    And the file "${outside}/web/F" does not exist

  Scenario: a local tag that differs from the remote's is not overwritten
    When I run the shell command "mkdir -p ${outside}/web ${outside}/remote.git && cd ${outside}/remote.git && git init -q --bare -b main && cd ${outside}/web && echo app > app.py && git init -q -b main && git add -A && git -c user.name=t -c user.email=t@example.com commit -q -m one && git tag v1 && git remote add origin ${outside}/remote.git && git push -q origin main && echo 2 > F && git add F && git -c user.name=t -c user.email=t@example.com commit -q -m two && git push -q origin main && git tag -f v2 && git push -q origin v2 && git tag -f v2 main~1 && git checkout -q main"
    Then the command succeeds
    When I run "stems repos checkout shop-api v2 --json"
    Then the command succeeds
    And the JSON at "$.data.detached" equals true
    And the file "${outside}/web/F" does not exist
    When I run the shell command "cd ${outside}/web && git rev-parse v2"
    Then the command succeeds

  @error
  Scenario: a path that is not a git repository is refused
    When I run the shell command "mkdir -p ${outside}/web"
    Then the command succeeds
    When I run "stems repos checkout shop-api main --json"
    Then the exit code is 1
    And the JSON error has code "REPO_NOT_GIT"
    And the JSON at "$.errors[0].message" contains "not a git repository"
