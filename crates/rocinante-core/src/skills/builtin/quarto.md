---
name: quarto
description: "Author and render Quarto (.qmd) documents: reproducible reports, HTML/PDF/Word output, revealjs slides, parameterized reports, websites and books, with executable Python/R cells. Use when asked to write a report from an analysis, render a .qmd, make slides from code, or publish results as a document."
---

# Quarto

Reproducible documents: markdown + executable code cells, rendered to HTML/PDF/Word/slides. Run with the `bash` tool.

1. **Check the install**: `quarto --version`. Missing → tell the user to install from https://quarto.org/docs/get-started/ (`brew install --cask quarto` on macOS); do not improvise a substitute renderer.

2. **Starter document** — write `report.qmd` with the `write` tool from this template:
````markdown
---
title: "FILL IN TITLE"
author: "FILL IN"
date: today
format:
  html:
    toc: true
    embed-resources: true      # single self-contained file
execute:
  echo: false                  # hide code, show results (reports)
  warning: false
---

## Summary

One paragraph of findings, written for the reader.

## Results

```{python}
#| label: fig-main
#| fig-cap: "Caption for the main figure."
import pandas as pd
import matplotlib.pyplot as plt
df = pd.read_csv("FILL_IN.csv")
df.plot.box()
plt.show()                     # inside Quarto cells plt.show() is CORRECT — output embeds
```

As @fig-main shows, ...

```{python}
from IPython.display import Markdown
Markdown(df.describe().to_markdown())
```
````
   R projects: use ```` ```{r} ```` cells (knitr engine picks up automatically) and `knitr::kable(df)` for tables.

3. **Render and preview:**
```bash
quarto render report.qmd --to html     # writes report.html — report the path
quarto render report.qmd --to docx
quarto render report.qmd --to pdf      # first time: quarto install tinytex
quarto preview report.qmd              # live-reload server for interactive editing only
```
   Never leave `quarto preview` running from an agent — render, report the output path, done.

4. **The features that cover most requests:**
   | Need | How |
   |---|---|
   | Cross-reference a figure/table | `#| label: fig-x` + `@fig-x` in text (tables: `tbl-x`) |
   | Parameterized report | `params:` in YAML; cells read `params$x` (R) / injected `params` dict (Python via `-P x:value` at render) |
   | Skip slow cells on re-render | `execute: freeze: auto` (project) or `#| cache: true` per cell |
   | Slides | `format: revealjs`; `##` starts a new slide |
   | Website/book | `quarto create project website mysite` — pages are .qmd files, `_quarto.yml` is the nav |
   | Show code too (tutorial style) | drop `echo: false`, add `code-fold: true` |

5. **Assembling an analysis report**: outputs from the EDA/ML/statistics skills (figures in `eda_output/`, `ml_output/`, tables printed as text) slot straight in — reference saved images with `![caption](eda_output/col_box.png)` or recompute inline in cells for full reproducibility (prefer inline when the data is small and local).

## Rules

- Inside .qmd cells, normal plotting rules invert: `plt.show()` / bare `ggplot(...)` at cell end is CORRECT — Quarto captures and embeds the output. The Agg/savefig rule applies only outside Quarto.
- `embed-resources: true` for HTML you'll send to someone — one file, no broken asset links.
- Don't commit render artifacts: `_site/`, `*_files/`, `.quarto/` belong in .gitignore.
- Render errors name the failing cell and line — fix that cell; `quarto render --execute-daemon-restart` clears a wedged kernel.
- Engine follows the project language: Python cells need `jupyter` (`python3 -m pip install jupyter`), R cells need `rmarkdown`/`knitr` installed in R.
