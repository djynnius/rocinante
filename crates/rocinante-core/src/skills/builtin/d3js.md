---
name: d3js
description: "Build data visualizations with D3.js v7: selections, data joins, scales, axes, loading CSV/JSON, bar/scatter/line charts in SVG. Use when asked to create a custom interactive chart, use D3, or build an SVG visualization beyond what a charting library offers."
---

# D3.js (v7)

Custom SVG charts. For a prototype, one plain HTML file is enough — no build step, no dev server; write it with the `write` tool and report the file path (the user opens it in a browser).

1. **HTML shell** — every prototype starts here:
```html
<!DOCTYPE html>
<meta charset="utf-8">
<div id="chart"></div>
<script src="https://cdn.jsdelivr.net/npm/d3@7"></script>
<script>
// chart code from the steps below goes here
</script>
```

2. **Margin convention** — copy verbatim; all drawing happens inside `g`:
```js
const margin = {top: 20, right: 20, bottom: 40, left: 50},
      width  = 640 - margin.left - margin.right,
      height = 400 - margin.top - margin.bottom;
const svg = d3.select("#chart").append("svg")
    .attr("width",  width + margin.left + margin.right)
    .attr("height", height + margin.top + margin.bottom)
  .append("g")
    .attr("transform", `translate(${margin.left},${margin.top})`);
```

3. **Scales map data → pixels; axes draw them:**
```js
const x = d3.scaleBand().domain(data.map(d => d.name)).range([0, width]).padding(0.15); // categories
// numeric x: d3.scaleLinear().domain(d3.extent(data, d => d.x)).nice().range([0, width])
const y = d3.scaleLinear().domain([0, d3.max(data, d => d.value)]).nice().range([height, 0]);
svg.append("g").attr("transform", `translate(0,${height})`).call(d3.axisBottom(x));
svg.append("g").call(d3.axisLeft(y));
```
   `range([height, 0])` for y — SVG's origin is top-left, so it is inverted on purpose.

4. **The data join** — one pattern for everything; `.join()` handles enter/update/exit:
```js
// bar chart
svg.selectAll("rect").data(data).join("rect")
    .attr("x", d => x(d.name))
    .attr("y", d => y(d.value))
    .attr("width", x.bandwidth())
    .attr("height", d => height - y(d.value))
    .attr("fill", "steelblue");

// scatter plot (numeric x scale)
svg.selectAll("circle").data(data).join("circle")
    .attr("cx", d => x(d.x)).attr("cy", d => y(d.y)).attr("r", 4);

// line chart
svg.append("path").datum(data)
    .attr("fill", "none").attr("stroke", "steelblue").attr("stroke-width", 1.5)
    .attr("d", d3.line().x(d => x(d.date)).y(d => y(d.value)));
```

5. **Load real data** (must run via a URL or local file next to the HTML):
```js
const data = await d3.csv("data.csv", d3.autoType);   // autoType converts numbers/dates
// const data = await d3.json("data.json");
```
   Wrap the chart code in `async` context or `.then(...)`. Browsers block `d3.csv` over `file://` in some setups — if data fails to load, inline it as a JS array or serve with `python3 -m http.server 8000`.

6. **Basic interactivity** — a title tooltip is one line: `.append("title").text(d => d.name)`. Hover styling: `.on("mouseover", (e, d) => d3.select(e.currentTarget).attr("fill", "tomato"))`.

## Rules

- D3 v7 syntax only: `.join("rect")` (not the old `.enter().append()` chains), event handlers receive `(event, d)`.
- Numbers from CSVs are strings unless `d3.autoType` (or explicit `+d.value`) is used — the #1 silent bug (charts render empty).
- Categorical x → `scaleBand`; numeric → `scaleLinear`; dates → `scaleTime`. Always `.nice()` numeric domains.
- Nothing renders: check the browser console first; then verify the svg exists (`document.querySelector("svg")`) and the scale domains are not `[undefined, undefined]`.
- If the request is a standard statistical chart with no custom interaction, use the `ggplot` skill instead (call the `skill` tool with `{"name": "ggplot"}`) — D3 is for bespoke visuals.
