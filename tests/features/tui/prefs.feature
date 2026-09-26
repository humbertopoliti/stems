@FR-UI-4
Feature: Persisted UI preferences
  `~/.config/stems/ui.toml` (or `STEMS_UI_CONFIG`) sets the theme, mouse,
  the default view and the refresh interval.

  Scenario: default_view = detail opens the detail view
    Given the "minimal" workspace is up
    And the file "ui.toml" is written with "default_view = 'detail'"
    When I run "stems attach --headless --script 'wait:healthy;frame'" with env STEMS_UI_CONFIG=${ws}/ui.toml
    Then the command succeeds
    And the last frame contains "[Detail]"
    And the last frame contains "Detail: echo-svc"

  Scenario: --view overrides the preference
    Given the "minimal" workspace is up
    And the file "ui.toml" is written with "default_view = 'detail'"
    When I run "stems attach --view table --headless --script 'wait:healthy;frame'" with env STEMS_UI_CONFIG=${ws}/ui.toml
    Then the command succeeds
    And the last frame contains "[Table]"

  Scenario: an invalid preferences file falls back to the defaults with a notice
    Given the "minimal" workspace is up
    And the file "ui.toml" is written with "theme = 'pink'"
    When I run "stems attach --headless --size 120x30 --script 'wait:healthy;frame'" with env STEMS_UI_CONFIG=${ws}/ui.toml
    Then the command succeeds
    And the last frame contains "[Table]"
    And the last frame contains "invalid"
