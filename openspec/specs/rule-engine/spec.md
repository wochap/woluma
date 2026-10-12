# Rule Engine Specification

## Purpose

Defines the rule condition grammar, first-match evaluation, settle and hysteresis semantics, unknown-ALS behaviour, and per-output active-rule tracking.

## Requirements

### Requirement: Condition grammar
A `when` table SHALL accept the keys `luma`, `als`, and `time`. `luma` and `als` values SHALL be strings with an explicit operator: `< N`, `<= N`, `> N`, `>= N`, `== N`, `!= N`, or an inclusive range `A..B`, where N, A, B are numbers in 0–100 and A ≤ B. A bare number without an operator SHALL be rejected. `time` SHALL be a string `HH:MM-HH:MM` in 24-hour local time.

#### Scenario: Valid comparison
- **WHEN** a rule has `when = { luma = ">= 70" }`
- **THEN** the config loads and the condition matches luma 70 and above

#### Scenario: Bare number rejected
- **WHEN** a rule has `when = { luma = 70 }` or `luma = "70"`
- **THEN** startup fails with an error stating an explicit operator is required

#### Scenario: Inverted range rejected
- **WHEN** a rule has `luma = "70..30"`
- **THEN** startup fails with an error naming the rule

#### Scenario: Unknown condition key
- **WHEN** a rule has `when = { battery = "< 20" }`
- **THEN** startup fails listing the valid keys `luma`, `als`, `time`

### Requirement: Time ranges use local machine time
A `time` condition SHALL match when the current local time is within `[start, end)`. When `end` is earlier than `start`, the range SHALL wrap past midnight. `start == end` SHALL be rejected.

#### Scenario: Daytime range
- **WHEN** `time = "07:00-21:00"` and local time is 14:30
- **THEN** the condition matches

#### Scenario: Wrapping range
- **WHEN** `time = "21:00-07:00"` and local time is 02:15
- **THEN** the condition matches

#### Scenario: End is exclusive
- **WHEN** `time = "21:00-07:00"` and local time is 07:00
- **THEN** the condition does not match

### Requirement: Conjunction and fallback
All keys within one `when` SHALL be combined with AND. A rule with an empty or absent `when` SHALL always match.

#### Scenario: All conditions must hold
- **WHEN** a rule has `when = { time = "21:00-07:00", luma = ">= 70" }`, local time is 22:00, and luma is 50
- **THEN** the rule does not match

#### Scenario: Fallback rule
- **WHEN** no earlier rule matches and the last rule has no `when`
- **THEN** the last rule becomes active

### Requirement: First match wins
For each output, rules SHALL be evaluated in config order and the first matching rule SHALL become that output's single active rule. If no rule matches, the output SHALL have no active rule and no hooks SHALL run.

#### Scenario: Most specific rule first
- **WHEN** rules are `night-bright` (time 21:00-07:00, luma >= 70), `bright` (luma >= 70), `default` (no when), at 22:00 with luma 85
- **THEN** `night-bright` is active and `bright` is not considered

#### Scenario: No rule matches
- **WHEN** the only rule is `bright` (luma >= 70) and luma is 20
- **THEN** the output has no active rule

### Requirement: Unknown ambient reading
An `als` condition SHALL NOT match while the ambient light reading is unknown, regardless of operator (including `!=`). The rest of the rule's conditions SHALL NOT rescue it.

#### Scenario: ALS unknown skips rule
- **WHEN** rules are `dim` (als < 20) then `default`, and the ambient reading is unknown
- **THEN** `default` is active

#### Scenario: Not-equal with unknown
- **WHEN** a rule has `als = "!= 50"` and the ambient reading is unknown
- **THEN** the rule does not match

### Requirement: Settle time
A change in input values SHALL only cause a different rule to become active after the newly matching rule has matched continuously for the configured `general.settle` duration. Time-condition boundaries SHALL take effect without settle delay.

#### Scenario: Brief content flash ignored
- **WHEN** `bright` is active, settle is 2s, and luma drops to 18 for 1s then returns to 88
- **THEN** `bright` stays active and no hooks run

#### Scenario: Sustained change applied
- **WHEN** `default` is active, settle is 2s, and luma rises to 88 and stays there for 2s
- **THEN** `bright` becomes active after 2s

### Requirement: Hysteresis keeps the active rule
While a rule is active, its `luma` and `als` conditions SHALL be evaluated with thresholds widened by the configured hysteresis in the direction that keeps the rule matching: `>=`/`>` lower the bound, `<=`/`<` raise it, ranges widen both edges, `==` matches within ±hysteresis, and `!=` excludes only the exact value. Rules that are not active SHALL use their exact thresholds. Evaluation order is unchanged, so an earlier non-active rule matching exactly still takes precedence.

#### Scenario: Staying in bright through a dip
- **WHEN** `bright` (luma >= 70) is active, luma hysteresis is 5, and luma drops to 67
- **THEN** `bright` remains active

#### Scenario: Leaving after the margin
- **WHEN** `bright` (luma >= 70) is active, luma hysteresis is 5, and luma drops to 64 for the settle duration
- **THEN** the next matching rule becomes active

#### Scenario: Entering uses exact threshold
- **WHEN** `mid` is active and `bright` (luma >= 70) is not, and luma rises to 68
- **THEN** `bright` does not become active

### Requirement: Re-evaluation triggers
Rules for an output SHALL be re-evaluated on every new luma sample for that output, on every ambient reading change, and at least once per minute for time conditions.

#### Scenario: Time boundary crossed with static content
- **WHEN** screen content is unchanged and local time passes 21:00
- **THEN** a rule with `time = "21:00-07:00"` becomes active within one minute
