@FR-LC-5 @recovery
Feature: one

  Scenario: plain
    Given x

  @NFR-2 @FR-XX-99
  Scenario: tagged
    Given y

  Rule: r
    @FR-WS-2
    Scenario Outline: outline <n>
      Given z
      @FR-WS-1
      Examples:
        | n |
        | 1 |
