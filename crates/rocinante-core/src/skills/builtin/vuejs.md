---
name: vuejs
description: "Build Vue 3 apps and pages that work offline — Rocinante bundles Vue 3.5.45, so no npm or internet is needed — or single-file components in an npm project: reactivity (ref/computed/watch), props and emits, v-model, lists and conditionals. Use when asked to build a Vue app, tool or page, write or fix a Vue component, or debug Vue reactivity."
---

# Vue 3

Composition API ONLY — never the Options API (`data()`, `methods:`) in new code. Follow this skill even if another Vue skill is installed: only this one has the bundled offline Vue.

**Pick the mode first.** Run `grep -l '"vue"' package.json 2>/dev/null` with the `bash` tool, then:

| What you find / what the user asked | Mode |
|---|---|
| The grep printed `package.json` (an existing Vue project) | **B — project** |
| The user asks for an npm or Vite project, a build step, `.vue` files, or URL routing | **B — project** (`npm` needs internet) |
| Anything else: a page, app, tool, form, dashboard, prototype, demo | **A — page** (bundled Vue, works offline) |

## A. Page mode — one HTML file, works offline

Rocinante ships Vue 3.5.45 as plain script files that define a global `Vue`. The line above these instructions ends with `Files it mentions live in <DIR>` — the Vue files are in that `<DIR>`: `vue.global.js` (development build — it reports mistakes, always build with it) and `vue.global.prod.js` (production build, step A5 only). The page needs no internet and opens by double-clicking `index.html`.

A1. **Create the folder and copy Vue into it** with the `bash` tool. Replace `FILL_IN_DIR` with the `<DIR>` path from the line above, `FILL_IN_FOLDER` with a new folder name (e.g. `todo-app`):
```bash
mkdir -p FILL_IN_FOLDER && cp "FILL_IN_DIR/vue.global.js" FILL_IN_FOLDER/ && ls -l FILL_IN_FOLDER
```
   If the copy fails, stop and report the exact error. Do not switch to a CDN.

A2. **Write `FILL_IN_FOLDER/index.html`** with the `write` tool. Copy this template exactly; change only `FILL_IN_TITLE` and the two `FILL_IN` blocks:
```html
<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>FILL_IN_TITLE</title>
<script>
  // Records any crash on the page so the step-A4 check can see it. Keep this first.
  addEventListener('error', e => document.documentElement.setAttribute('data-error', e.message));
</script>
<style>
  body { font-family: system-ui, sans-serif; max-width: 40rem; margin: 2rem auto; padding: 0 1rem; }
</style>
</head>
<body>
<div id="app">
  <!-- FILL_IN: the page. Use refs here WITHOUT .value -->
  <h1>{{ title }}</h1>
  <button @click="count++">Clicked {{ count }} times</button>
</div>
<script src="vue.global.js"></script>
<script>
const { createApp, ref, reactive, computed, watch, onMounted } = Vue;

const app = createApp({
  setup() {
    // FILL_IN: state and functions. Refs need .value here.
    const title = ref('FILL_IN_TITLE');
    const count = ref(0);
    // Every name the page uses MUST be listed in this return.
    return { title, count };
  }
});

// These two lines let the step-A4 check see Vue's errors and warnings. Keep them.
app.config.errorHandler = (err) => { console.error(err); document.documentElement.setAttribute('data-error', String(err)); };
app.config.warnHandler = (msg) => { console.warn(msg); document.documentElement.setAttribute('data-warn', msg); };
app.mount('#app');
</script>
</body>
</html>
```

A3. **Building blocks** — the HTML part goes in the `#app` block, the JS part in `setup()`, and every new name goes in the `return`.

   **List with add and remove:**
```html
<form @submit.prevent="add">
  <input v-model="draft" placeholder="New item">
  <button>Add</button>
</form>
<ul>
  <li v-for="item in items" :key="item.id">
    {{ item.text }} <button @click="remove(item.id)">✕</button>
  </li>
</ul>
<p v-if="items.length === 0">Nothing yet.</p>
<p v-else>{{ remaining }} item(s)</p>
```
```js
const draft = ref('');
const items = ref([]);
let nextId = 1;
function add() {
  if (!draft.value.trim()) return;
  items.value.push({ id: nextId++, text: draft.value.trim() });
  draft.value = '';
}
function remove(id) {
  items.value = items.value.filter(item => item.id !== id);
}
const remaining = computed(() => items.value.length);
// return { draft, items, add, remove, remaining }
```

   **Remember between visits** — replace the two lines `const items = ref([]);` and `let nextId = 1;` with:
```js
const items = ref(JSON.parse(localStorage.getItem('FILL_IN_KEY') || '[]'));
watch(items, value => localStorage.setItem('FILL_IN_KEY', JSON.stringify(value)), { deep: true });
let nextId = Math.max(0, ...items.value.map(item => item.id)) + 1;
```

   **A reusable piece** (only when the same markup repeats 3+ times). Register it after `createApp(...)` and before `app.mount`:
```js
app.component('stat-card', {
  props: { label: String, value: Number },
  template: `<div class="card"><strong>{{ value }}</strong> {{ label }}</div>`
});
```
   Use it in the page as `<stat-card label="Users" :value="42"></stat-card>` — lowercase-with-dashes and a closing tag, never `<StatCard />`.

   **Data from a CSV file** — never `fetch()` it (a double-clicked page cannot read files). Convert it to `data.js` with the `bash` tool:
```bash
python3 - FILL_IN_DATA.csv FILL_IN_FOLDER/data.js <<'EOF'
import csv, json, sys
def num(v):
    try:
        return float(v)
    except ValueError:
        return v
with open(sys.argv[1], newline="", encoding="utf-8") as f:
    rows = [{k: num(v) for k, v in row.items()} for row in csv.DictReader(f)]
with open(sys.argv[2], "w", encoding="utf-8") as f:
    f.write("const data = " + json.dumps(rows) + ";\n")
print(len(rows), "rows; columns:", list(rows[0]) if rows else [])
EOF
```
   Add `<script src="data.js"></script>` on the line right after `<script src="vue.global.js"></script>`, then in `setup()` write `const rows = ref(data);` and return `rows`.

A4. **Check the page in a headless browser** with the `bash` tool (copy exactly; replace only `FILL_IN_FOLDER`):
```bash
cd FILL_IN_FOLDER
B=""
for c in google-chrome google-chrome-stable chromium chromium-browser microsoft-edge \
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" \
    "/Applications/Chromium.app/Contents/MacOS/Chromium" \
    "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge" \
    "/c/Program Files/Google/Chrome/Application/chrome.exe" \
    "/c/Program Files (x86)/Microsoft/Edge/Application/msedge.exe"; do
  command -v "$c" >/dev/null 2>&1 && { B="$c"; break; }
done
if [ -z "$B" ]; then echo "NO_BROWSER"; else
  P=$(pwd -W 2>/dev/null || pwd)
  "$B" --headless --disable-gpu --virtual-time-budget=3000 \
      --dump-dom "file:///${P#/}/index.html" > .page-check.html 2>/dev/null
  if grep -q 'data-error=\|data-warn=' .page-check.html; then grep -o 'data-error="[^"]*"\|data-warn="[^"]*"' .page-check.html
  elif grep -q 'data-v-app' .page-check.html; then echo "PAGE_OK"
  else echo "NOT_MOUNTED"; fi
  rm -f .page-check.html
fi
```
   | Output | Do this |
   |---|---|
   | `PAGE_OK` | The page works — stop checking. Go straight to A5: no screenshots, no test scripts, no driving the page. |
   | `data-warn="Property &quot;x&quot; was accessed during render but is not defined..."` | `x` is missing from setup()'s `return` (or misspelled). Add it, run A4 again. |
   | any other `data-error="..."` / `data-warn="..."` | Fix that exact problem in `index.html` with the `edit` tool, run A4 again. After 3 failed fixes, stop and report it. |
   | `NOT_MOUNTED` | Vue never started: check `vue.global.js` is in the folder (`ls FILL_IN_FOLDER`) and the script order matches the template, then run A4 again. |
   | `NO_BROWSER` (or the command itself fails) | Skip the check; say "not checked in a browser" in A6. |

A5. **Only if the user says the page will be hosted or shared online**: run `cp "FILL_IN_DIR/vue.global.prod.js" FILL_IN_FOLDER/` and change `<script src="vue.global.js">` to `<script src="vue.global.prod.js">` with the `edit` tool. Otherwise skip this step.

A6. **Report**: the full path of `index.html`; "double-click to open — no internet needed"; and the A4 result.

## B. Project mode — npm and single-file components

For deeper project guidance (Pinia, Vue Router, testing), a user-installed skill may exist: call the `skill` tool with `{"name": "vue"}`; if it loads, use it alongside these steps.

B1. **Scaffold / run** (existing project: check `package.json` scripts first):
```bash
npm create vue@latest my-app -- --typescript   # scaffold; answer the prompts
cd my-app && npm install && npm run dev        # dev server, prints the local URL
npm run build                                  # production build to dist/
```

B2. **SFC skeleton** — every component is this shape:
```vue
<script setup>
import { ref, computed, watch } from "vue"

const count = ref(0)                          // mutable state: .value in JS, bare in template
const doubled = computed(() => count.value * 2)
watch(count, (now, before) => console.log(before, "->", now))
</script>

<template>
  <button @click="count++">{{ count }} ({{ doubled }})</button>
</template>

<style scoped>
button { padding: 0.5rem 1rem; }
</style>
```
   Reactivity rules: `ref()` for single values (access with `.value` in script, without in template); `reactive()` for objects you never reassign; a destructured `reactive` loses reactivity — don't destructure it.

B3. **Props, emits, v-model:**
```vue
<script setup>
const props = defineProps({ title: { type: String, required: true }, items: Array })
const emit = defineEmits(["select"])
const model = defineModel()                    // two-way binding: parent uses v-model
</script>

<template>
  <li v-for="item in props.items" :key="item.id" @click="emit('select', item.id)">
    {{ item.name }}
  </li>
  <input v-model="model" />
</template>
```
   `v-for` ALWAYS gets a `:key` bound to a stable id — never the array index if the list reorders.

B4. **Communication — pick from this table:**
   | Between | Mechanism |
   |---|---|
   | Parent → child | props |
   | Child → parent | `emit` events |
   | Two-way form value | `defineModel` / `v-model` |
   | Distant components | Pinia store (`npm i pinia`) or `provide`/`inject` |

B5. **Conditionals:** `v-if` removes from the DOM (use for rarely-shown blocks), `v-show` toggles CSS display (use for frequent toggles). `v-else` must be the immediately following sibling.

## Rules

- Composition API only: `setup()` in page mode, `<script setup>` in project mode. If an existing codebase is Options API, match it for small fixes; say so in the report.
- Forgot `.value` is the #1 bug: in JavaScript refs need `.value`; in the page/template they must NOT have it.
- Page mode is offline: never write `import`, `type="module"`, an import map, `<script src="https://...">`, `.vue` files or `<script setup>` there — use the global `Vue` from `<script src="vue.global.js"></script>`.
- Page mode: every ref, computed and function the page uses must be in setup()'s `return { ... }` — a missing name shows up blank.
- Page mode: A4 is the whole check. Never write test scripts or harnesses (no `.mjs`/`.py` checkers, no browser profiles) — not in `FILL_IN_FOLDER`, not in `/tmp`. Never edit `vue.global.js`; never delete the error-recording lines from the template.
- `v-for` always has `:key` with a stable id.
- Component files PascalCase (`ItemList.vue`), used as `<ItemList />` — project mode only; page mode uses `<item-list></item-list>`.
- Mutating a prop is an error — emit an event or use `defineModel` instead.
- Blank page after `npm run dev`: open the browser console via the terminal output URL; the error is almost always a bad import path or missing `:key`.
- For a full framework app (routing, SSR) prefer the user-installed `nuxt`/`vue` skills when present (`skill` tool); this skill covers page- and component-level Vue.
- 3D graphics or 3D charts: call the `skill` tool with `{"name": "threejs"}`.
