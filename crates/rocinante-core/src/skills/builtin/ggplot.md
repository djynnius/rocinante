---
name: ggplot
description: "Grammar-of-graphics plotting with ggplot2 (R) and plotnine (Python). Use when asked to make publication-quality charts — bar, box, scatter, line, histogram, faceted small multiples — style or theme a plot, or translate a ggplot between R and Python."
---

# ggplot (R ggplot2 / Python plotnine)

Same grammar in both languages: data → `aes` mappings → geoms → facets → labels → theme. Pick the language the project already uses (R if `.R`/`renv.lock` present, else Python with plotnine: `python3 -m pip install plotnine`). Run with the `bash` tool; ALWAYS save to a file, never open a window.

1. **Skeleton.** Every plot is this shape:

   R:
   ```r
   library(ggplot2)
   p <- ggplot(df, aes(x = xcol, y = ycol)) + geom_point()
   ggsave("plots/name.png", p, width = 8, height = 5, dpi = 150)
   ```
   Python:
   ```python
   from plotnine import *
   p = ggplot(df, aes(x="xcol", y="ycol")) + geom_point()
   p.save("plots/name.png", width=8, height=5, dpi=150)
   ```
   `mkdir -p plots` first; report the saved path.

2. **Pick the geom from the data types:**

   | Data | Geom |
   |---|---|
   | one categorical (counts) | `geom_bar()`; pre-computed heights → `geom_col()` |
   | one numeric | `geom_histogram(bins=30)` |
   | numeric by category | `geom_boxplot()` |
   | numeric vs numeric | `geom_point()`; add trend `+ geom_smooth(method="lm")` |
   | value over time | `geom_line()` |

3. **Grouping and color:** put it in the aes — R `aes(x, y, color = group, fill = group)`, Python `aes(..., color="group", fill="group")`. Gotcha: a numeric code used as a category must be wrapped — R `factor(x)`, Python `"factor(x)"` — or you get a gradient instead of discrete colors.

4. **Small multiples:** `+ facet_wrap(~ group)` (R) / `+ facet_wrap("~group")` (Python). Free axes when scales differ: `facet_wrap(~g, scales = "free_y")`.

5. **Labels and readability** (always do this):
   ```r
   + labs(title = "…", x = "Unit (u)", y = "Unit (u)", color = "Legend title")
   + theme_minimal()
   ```
   Long category names → flip: `+ coord_flip()`. Rotate ticks: `+ theme(axis_text_x=element_text(angle=45, hjust=1))` (plotnine) / `axis.text.x = element_text(angle = 45, hjust = 1)` (R).

6. **Combined example** — grouped boxplot as used in EDA:

   ```python
   p = (ggplot(df, aes(x="factor(group)", y="value", fill="factor(group)"))
        + geom_boxplot()
        + labs(title="Value by group", x="Group", y="Value (unit)")
        + theme_minimal())
   p.save("plots/value_by_group.png", width=8, height=5, dpi=150)
   ```

## Rules

- Never call `print(p)`/`plot()` interactively or rely on a display; the terminal has none. Save with `ggsave()` / `.save()` and report the file path.
- plotnine quirk: everything inside `aes()` is a STRING (`aes(x="col")`); in R they are bare names (`aes(x = col)`). Do not mix the two styles.
- One message per plot decision: state which geom and why (which table row in step 2).
- Package missing: R → `Rscript -e 'install.packages("ggplot2", repos="https://cloud.r-project.org")'`; Python → `python3 -m pip install plotnine`.
- If the request is a quick diagnostic rather than a styled figure, matplotlib/`df.plot` from the EDA skill is fine — this skill is for presentable output.
