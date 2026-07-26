---
name: vuejs
description: "Build Vue 3 components and apps with the Composition API: single-file components, reactivity (ref/computed/watch), props and emits, v-model, lists and conditionals. Use when asked to write or fix a Vue component, build a Vue app or page, or debug Vue reactivity."
---

# Vue 3

Composition API with `<script setup>` ONLY — never the Options API (`data()`, `methods:`) in new code. First check whether a richer user-installed skill exists: call the `skill` tool with `{"name": "vue"}`; if it loads, follow that instead.

1. **Scaffold / run** (existing project: check `package.json` scripts first):
```bash
npm create vue@latest my-app -- --typescript   # scaffold; answer the prompts
cd my-app && npm install && npm run dev        # dev server, prints the local URL
npm run build                                  # production build to dist/
```

2. **SFC skeleton** — every component is this shape:
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

3. **Props, emits, v-model:**
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

4. **Communication — pick from this table:**
   | Between | Mechanism |
   |---|---|
   | Parent → child | props |
   | Child → parent | `emit` events |
   | Two-way form value | `defineModel` / `v-model` |
   | Distant components | Pinia store (`npm i pinia`) or `provide`/`inject` |

5. **Conditionals:** `v-if` removes from the DOM (use for rarely-shown blocks), `v-show` toggles CSS display (use for frequent toggles). `v-else` must be the immediately following sibling.

## Rules

- `<script setup>` + Composition API only. If the existing codebase is Options API, match it for small fixes; say so in the report.
- Forgot `.value` is the #1 bug: in `<script>` refs need `.value`; in `<template>` they must NOT have it.
- Component files PascalCase (`ItemList.vue`), used as `<ItemList />`.
- Mutating a prop is an error — emit an event or use `defineModel` instead.
- Blank page after `npm run dev`: open the browser console via the terminal output URL; the error is almost always a bad import path or missing `:key`.
- For a full framework app (routing, SSR) prefer the user-installed `nuxt`/`vue` skills when present (`skill` tool); this skill covers component-level Vue.
