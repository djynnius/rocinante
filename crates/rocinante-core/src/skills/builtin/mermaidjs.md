---
name: mermaidjs
description: "Write Mermaid diagrams: flowcharts, sequence diagrams, class diagrams, ER diagrams, state machines, Gantt charts. Use when asked to diagram an architecture, flow, schema, or process in markdown, create a mermaid diagram, or render one to an image."
---

# Mermaid

Text-to-diagram. Pick the diagram type from the table, copy its template, adapt. Mermaid renders automatically in GitHub/GitLab markdown and most doc sites — deliver the fenced block; only render to an image when explicitly asked (step 3).

1. **Pick the type:**
   | Need | Type |
   |---|---|
   | Process / decision flow, architecture boxes | `flowchart` |
   | Who-calls-whom over time (APIs, protocols) | `sequenceDiagram` |
   | Class/type structure | `classDiagram` |
   | Database schema | `erDiagram` |
   | State machine / lifecycle | `stateDiagram-v2` |
   | Project timeline | `gantt` |

2. **Templates** — copy exactly, then adapt:

   ```mermaid
   flowchart TD
       A[Start] --> B{Valid?}
       B -->|yes| C[Process]
       B -->|no| D[Reject]
       C --> E[(Database)]
       subgraph API
           B; C
       end
   ```
   `TD` top-down, `LR` left-right. Shapes: `[box]`, `{diamond}`, `([rounded])`, `[(cylinder)]`.

   ```mermaid
   sequenceDiagram
       participant U as User
       participant S as Server
       U->>S: POST /login
       activate S
       S-->>U: 200 + token
       deactivate S
       Note over U,S: token stored client-side
   ```

   ```mermaid
   classDiagram
       Animal <|-- Dog
       Animal : +String name
       Animal : +speak()
       class Dog {
           +fetch()
       }
   ```

   ```mermaid
   erDiagram
       USER ||--o{ ORDER : places
       ORDER ||--|{ LINE_ITEM : contains
       USER {
           int id PK
           string email
       }
   ```
   Cardinality: `||` exactly one, `o{` zero-or-more, `|{` one-or-more.

   ```mermaid
   stateDiagram-v2
       [*] --> Idle
       Idle --> Running : start
       Running --> Idle : stop
       Running --> [*] : crash
   ```

   ```mermaid
   gantt
       dateFormat YYYY-MM-DD
       title Rollout
       section Build
           Implement :a1, 2026-08-01, 10d
           Test      :after a1, 5d
   ```

3. **Render to an image** only when a file is required:
```bash
npx -y @mermaid-js/mermaid-cli -i diagram.mmd -o diagram.png
```
   (Write the bare diagram text — no ``` fences — into `diagram.mmd` with the `write` tool first. Needs node; check `node --version`.)

## Rules

- Label text containing `(`, `)`, `:`, `,`, or starting with a number MUST be quoted: `A["Retry (3x)"]`. Unquoted specials are the #1 parse error.
- Node ids are not labels: `A[Label]` — reuse `A` for edges, never the bracket text.
- One diagram per fenced block; the first word of the block must be the diagram type.
- Keep flowcharts under ~20 nodes; beyond that split into linked diagrams with a `subgraph` overview.
- Parse error when rendering: bisect — delete half the lines, re-render, repeat until the bad line is found; it is almost always quoting.
