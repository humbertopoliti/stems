@FR-WS-3
Feature: repos sync fetches and checks out the configured ref
  `stems repos sync` fetches every git codebase and checks out its `ref`
  (a branch, a tag or a sha) when the working tree is clean. It works
  without a daemon; with one it runs there and emits `repo.*` events.

  Scenario: changing ref to a new tag checks it out
    Given the fixture workspace "git-codebase"
    And a bare git repository "shop" made from "examples/repos/shop-api"
    And a local override setting stems.shop-api.codebase.git=${var:shop_url}
    When I run "stems repos sync --json"
    Then the command succeeds
    And the JSON at "$.data.repos[0].action" equals "cloned"
    And the JSON at "$.data.repos[0].path" contains ".stems/repos/shop-api"
    When I run the shell command "cd ${var:shop_src} && echo 2.0.0 > VERSION && git add VERSION && git commit -q -m v2 && git tag v2 && git push -q origin main v2 && printf '{\042sha\042: \042%s\042}' $(git rev-parse v2)"
    Then the command succeeds
    When I save the JSON at "$.sha" as "v2_sha"
    Given a local override setting stems.shop-api.codebase.ref=v2
    When I run "stems repos sync --json"
    Then the command succeeds
    And the JSON at "$.data.repos[0].action" equals "checked_out"
    And the JSON at "$.data.repos[0].ref" equals "v2"
    And the JSON at "$.data.repos[0].sha" equals "${var:v2_sha}"
    And the file ".stems/repos/shop-api/VERSION" contains "2.0.0"
    When I run "stems repos status --json"
    Then the command succeeds
    And the JSON at "$.data.repos[0].sha" equals "${var:v2_sha}"
    And the JSON at "$.data.repos[0].ref" equals "v2"
    And the JSON at "$.data.repos[0].branch" equals null
    When I run "stems repos sync --json"
    Then the JSON at "$.data.repos[0].action" equals "up_to_date"

  Scenario: with a daemon running, sync emits repo events
    Given the fixture workspace "git-codebase"
    And a bare git repository "shop" made from "examples/repos/shop-api"
    And a local override setting stems.shop-api.codebase.git=${var:shop_url}
    And the daemon is started
    When I run "stems repos sync shop-api --json"
    Then the command succeeds
    And the JSON at "$.data.repos[0].action" equals "cloned"
    And within 5s the events stream contains {"kind": "repo.cloned", "stem": "shop-api"}
    When I run the shell command "cd ${var:shop_src} && echo 2.0.0 > VERSION && git add VERSION && git commit -q -m v2 && git tag v2 && git push -q origin main v2"
    Then the command succeeds
    Given a local override setting stems.shop-api.codebase.ref=v2
    When I run "stems repos sync --json"
    Then the command succeeds
    And the JSON at "$.data.repos[0].action" equals "checked_out"
    And within 5s the events stream contains {"kind": "repo.checked_out", "stem": "shop-api", "data": {"ref": "v2"}}
