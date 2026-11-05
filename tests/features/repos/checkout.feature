@FR-WS-3
Feature: repos checkout moves a git codebase to any ref; sync --reset goes back
  `stems repos checkout <stem> <ref>` fetches, then checks out a branch
  (as `origin/<branch>`), a tag or a sha, detached. It refuses a tree with
  uncommitted changes (`REPO_DIRTY`) unless `--force` (the changes are
  carried over, as `git checkout` does). Nothing is recorded: `repos
  status` shows the declared `ref`, what is `checked_out` and `drift:
  true`; `repos sync` treats the tree as moved by the developer and leaves
  it alone until `repos sync --reset <stem>` (deliverable 49).

  Scenario: check out a feature branch and a sha, then reset to the declared ref
    Given the fixture workspace "git-codebase"
    And a bare git repository "shop" made from "examples/repos/shop-api"
    And a local override setting stems.shop-api.codebase.git=${var:shop_url}
    When I run "stems repos sync --json"
    Then the command succeeds
    And the JSON at "$.data.repos[0].action" equals "cloned"
    When I run "stems repos status --json"
    Then the JSON at "$.data.repos[0].drift" equals false
    And the JSON at "$.data.repos[0].checked_out" equals "main"
    When I run the shell command "cd ${var:shop_src} && git checkout -q -b feature && echo 3.0.0 > VERSION && git add VERSION && git commit -q -m feature && git push -q origin feature && printf '{\042sha\042: \042%s\042}' $(git rev-parse HEAD)"
    Then the command succeeds
    When I save the JSON at "$.sha" as "feature_sha"
    When I run "stems repos checkout shop-api feature --json"
    Then the command succeeds
    And the JSON at "$.data.stem" equals "shop-api"
    And the JSON at "$.data.ref" equals "feature"
    And the JSON at "$.data.sha" equals "${var:feature_sha}"
    And the file ".stems/repos/shop-api/VERSION" contains "3.0.0"
    When I run "stems repos status --json"
    Then the JSON at "$.data.repos[0].ref" equals "main"
    And the JSON at "$.data.repos[0].checked_out" equals "origin/feature"
    And the JSON at "$.data.repos[0].drift" equals true
    And the JSON at "$.data.repos[0].branch" equals null
    When I run "stems repos sync --json"
    Then the command succeeds
    And the JSON at "$.data.repos[0].action" equals "skipped_dirty"
    And the JSON at "$.data.repos[0].message" contains "--reset"
    And the file ".stems/repos/shop-api/VERSION" contains "3.0.0"
    When I run "stems repos sync shop-api --reset --json"
    Then the command succeeds
    And the JSON at "$.data.repos[0].action" equals "checked_out"
    And the file ".stems/repos/shop-api/VERSION" does not exist
    When I run "stems repos status --json"
    Then the JSON at "$.data.repos[0].drift" equals false
    And the JSON at "$.data.repos[0].branch" equals "main"
    When I run "stems repos checkout shop-api ${var:feature_sha} --json"
    Then the command succeeds
    And the JSON at "$.data.sha" equals "${var:feature_sha}"
    When I run "stems repos status --json"
    Then the JSON at "$.data.repos[0].drift" equals true

  Scenario: a dirty tree is refused; --force carries the change over
    Given the fixture workspace "git-codebase"
    And a bare git repository "shop" made from "examples/repos/shop-api"
    And a local override setting stems.shop-api.codebase.git=${var:shop_url}
    And the daemon is started
    When I run "stems repos sync --json"
    Then the command succeeds
    When I run the shell command "cd ${var:shop_src} && git tag v2 && git push -q origin v2"
    Then the command succeeds
    Given the file ".stems/repos/shop-api/app.py" is written with "# my local work"
    When I run "stems repos checkout shop-api v2 --json"
    Then the exit code is 1
    And the JSON error has code "REPO_DIRTY"
    And the JSON at "$.errors[0].hint" contains "--force"
    And the file ".stems/repos/shop-api/app.py" contains "# my local work"
    When I run "stems repos checkout shop-api v2 --force --json"
    Then the command succeeds
    And the JSON at "$.data.ref" equals "v2"
    And the file ".stems/repos/shop-api/app.py" contains "# my local work"
    And within 5s the events stream contains {"kind": "repo.checked_out", "stem": "shop-api", "data": {"ref": "v2"}}
    When I run "stems repos status --json"
    Then the JSON at "$.data.repos[0].checked_out" equals "v2"
    And the JSON at "$.data.repos[0].drift" equals true

  @error
  Scenario: unpushed commits on HEAD are refused unless --force; an option-like ref is refused
    Given the fixture workspace "git-codebase"
    And a bare git repository "shop" made from "examples/repos/shop-api"
    And a local override setting stems.shop-api.codebase.git=${var:shop_url}
    When I run "stems repos sync --json"
    Then the command succeeds
    When I run the shell command "cd ${var:shop_src} && git tag v2 && git push -q origin v2"
    Then the command succeeds
    When I run the shell command "cd .stems/repos/shop-api && echo local > LOCAL && git add LOCAL && git -c user.name=t -c user.email=t@example.com commit -q -m local"
    Then the command succeeds
    When I run "stems repos checkout shop-api v2 --json"
    Then the exit code is 1
    And the JSON error has code "REPO_DIRTY"
    And the JSON at "$.errors[0].details.unpushed_commits" equals true
    And the JSON at "$.errors[0].hint" contains "reflog"
    And the file ".stems/repos/shop-api/LOCAL" contains "local"
    When I run "stems repos checkout shop-api v2 --force --json"
    Then the command succeeds
    And the JSON at "$.data.ref" equals "v2"
    And the file ".stems/repos/shop-api/LOCAL" does not exist
    When I run "stems repos checkout shop-api --json -- --orphan=x"
    Then the exit code is 2
    And the JSON error has code "USAGE"
