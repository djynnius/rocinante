---
name: threejs
description: "Build 3D scenes and 3D charts/plots with three.js, offline: Rocinante bundles three.js r187, so the page needs no internet and opens with a double-click. Use when asked for 3D graphics, a 3D bar chart, 3D scatter or surface plot, a rotating or orbitable object, a WebGL scene, or anything with three.js."
---

# three.js (r187, bundled — works offline)

Rocinante ships three.js as ONE file, `three.min.js`, that defines a global `THREE` (including `THREE.OrbitControls`, `THREE.CSS2DRenderer`, `THREE.CSS2DObject`). The line above these instructions ends with `Files it mentions live in <DIR>` — `three.min.js` is in that `<DIR>`. A page built this way works with no internet and opens by double-clicking `index.html`. At every **ASK** step: ask the user with your recommendation; if you are running as a subagent, stop and return the question + recommendation in your report.

1. **Create the folder and copy the bundled file into it** with the `bash` tool. Replace `FILL_IN_DIR` with the `<DIR>` path from the line above, `FILL_IN_FOLDER` with a new folder name for this visual (e.g. `sales-3d`):
```bash
mkdir -p FILL_IN_FOLDER && cp "FILL_IN_DIR/three.min.js" FILL_IN_FOLDER/ && ls -l FILL_IN_FOLDER
```
   If the copy fails, stop and report the exact error. Do not switch to a CDN.

2. **Write `FILL_IN_FOLDER/index.html`** with the `write` tool. Copy this template exactly; change only `FILL_IN_TITLE` and the `FILL_IN` comment block:
```html
<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>FILL_IN_TITLE</title>
<script>
  // Records any crash on the page so the step-5 check can see it. Keep this first.
  addEventListener('error', e => document.documentElement.setAttribute('data-error', e.message));
</script>
<style>
  html, body { margin: 0; height: 100%; overflow: hidden; background: #111; }
  .label { color: #fff; font: 13px sans-serif; background: rgba(0, 0, 0, 0.6); padding: 2px 6px; border-radius: 3px; }
</style>
</head>
<body>
<script src="three.min.js"></script>
<script>
const scene = new THREE.Scene();
scene.background = new THREE.Color(0x111111);

const camera = new THREE.PerspectiveCamera(50, innerWidth / innerHeight, 0.1, 1000);
camera.position.set(8, 6, 10);

const renderer = new THREE.WebGLRenderer({ antialias: true });
renderer.setPixelRatio(devicePixelRatio);
renderer.setSize(innerWidth, innerHeight);
document.body.appendChild(renderer.domElement);

const labelRenderer = new THREE.CSS2DRenderer();
labelRenderer.setSize(innerWidth, innerHeight);
labelRenderer.domElement.style.position = 'absolute';
labelRenderer.domElement.style.top = '0';
labelRenderer.domElement.style.pointerEvents = 'none';
document.body.appendChild(labelRenderer.domElement);

const controls = new THREE.OrbitControls(camera, renderer.domElement);
controls.enableDamping = true;

scene.add(new THREE.AmbientLight(0xffffff, 0.6));
const sun = new THREE.DirectionalLight(0xffffff, 2);
sun.position.set(5, 10, 7);
scene.add(sun);

// ===== FILL_IN: paste the building blocks from step 3 here =====

addEventListener('resize', () => {
  camera.aspect = innerWidth / innerHeight;
  camera.updateProjectionMatrix();
  renderer.setSize(innerWidth, innerHeight);
  labelRenderer.setSize(innerWidth, innerHeight);
});

renderer.setAnimationLoop(() => {
  // per-frame animation goes here, e.g. mesh.rotation.y += 0.01;
  controls.update();
  renderer.render(scene, camera);
  labelRenderer.render(scene, camera);
});
</script>
</body>
</html>
```

3. **Pick the building blocks** and paste them at the `FILL_IN` comment. Replace the example values marked `FILL_IN`.

   **3D bar chart** (bars scaled so the tallest is 5 units, each with a label):
```js
const data = [{ label: 'A', value: 3 }, { label: 'B', value: 5 }, { label: 'C', value: 2 }]; // FILL_IN
const maxValue = Math.max(...data.map(d => d.value));
data.forEach((d, i) => {
  const height = (d.value / maxValue) * 5;
  const bar = new THREE.Mesh(
    new THREE.BoxGeometry(0.8, height, 0.8),
    new THREE.MeshStandardMaterial({ color: new THREE.Color().setHSL(i / data.length, 0.6, 0.55) })
  );
  bar.position.set((i - (data.length - 1) / 2) * 1.2, height / 2, 0);
  scene.add(bar);
  const tag = document.createElement('div');
  tag.className = 'label';
  tag.textContent = d.label + ': ' + d.value;
  const label = new THREE.CSS2DObject(tag);
  label.position.set(0, height / 2 + 0.3, 0);
  bar.add(label);
});
scene.add(new THREE.GridHelper(10, 10, 0x444444, 0x222222));
```

   **3D scatter plot** (one `Points` object — fast up to 100,000 points):
```js
const rows = [[1, 2, 3], [2, 1, 0], [-1, 0.5, 2]]; // FILL_IN: [x, y, z] per point, already scaled to -5..5
const geometry = new THREE.BufferGeometry();
geometry.setAttribute('position', new THREE.Float32BufferAttribute(rows.flat(), 3));
scene.add(new THREE.Points(geometry, new THREE.PointsMaterial({ color: 0xffcc00, size: 0.2 })));
scene.add(new THREE.AxesHelper(5)); // x = red, y = green, z = blue
```

   **Surface plot** (height `y = f(x, z)`):
```js
const surface = new THREE.PlaneGeometry(10, 10, 60, 60);
surface.rotateX(-Math.PI / 2); // lay it flat: x right, z toward the viewer, y up
const pos = surface.attributes.position;
for (let i = 0; i < pos.count; i++) {
  const x = pos.getX(i), z = pos.getZ(i);
  pos.setY(i, Math.sin(x) * Math.cos(z)); // FILL_IN: your y = f(x, z)
}
surface.computeVertexNormals();
scene.add(new THREE.Mesh(surface, new THREE.MeshStandardMaterial({ color: 0x3fa7d6, side: THREE.DoubleSide })));
```

   **Line through points:**
```js
const path = [[0, 0, 0], [1, 2, 0], [2, 1, 1]]; // FILL_IN
const lineGeometry = new THREE.BufferGeometry().setFromPoints(path.map(p => new THREE.Vector3(...p)));
scene.add(new THREE.Line(lineGeometry, new THREE.LineBasicMaterial({ color: 0x66ffcc })));
```

   **A shape** (spin it by adding `mesh.rotation.y += 0.01;` inside `setAnimationLoop`):
```js
const mesh = new THREE.Mesh(new THREE.BoxGeometry(2, 2, 2), new THREE.MeshStandardMaterial({ color: 0x4f9dff }));
scene.add(mesh);
// other shapes: new THREE.SphereGeometry(1, 32, 16), new THREE.CylinderGeometry(1, 1, 2, 32),
//               new THREE.TorusGeometry(1, 0.3, 16, 64), new THREE.ConeGeometry(1, 2, 32)
```

4. **Data from a file** (skip if the data is typed into the code). Never `fetch()` it — a double-clicked page cannot read files. Convert it to `data.js` with the `bash` tool instead:
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
   For a `.json` file: `{ printf 'const data = '; cat FILL_IN_DATA.json; printf ';\n'; } > FILL_IN_FOLDER/data.js`. Then, in `index.html`, add `<script src="data.js"></script>` on the line right after `<script src="three.min.js"></script>`, and delete the example `const data = [...]` line from step 3. Use the printed column names in place of `d.label` / `d.value`.

5. **Check the page in a headless browser** with the `bash` tool (copy exactly; replace only `FILL_IN_FOLDER`):
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
  "$B" --headless --disable-gpu --enable-unsafe-swiftshader --virtual-time-budget=3000 \
      --dump-dom "file:///${P#/}/index.html" > .page-check.html 2>/dev/null
  if grep -q 'data-error=' .page-check.html; then grep -o 'data-error="[^"]*"' .page-check.html
  elif grep -q '<canvas' .page-check.html; then echo "PAGE_OK"
  else echo "NO_CANVAS"; fi
  rm -f .page-check.html
fi
```
   | Output | Do this |
   |---|---|
   | `PAGE_OK` | The page works — stop checking. Go straight to step 6: no screenshots, no test scripts, no image or pixel checks. |
   | `data-error="..."` | Fix that exact error in `index.html` with the `edit` tool, then run step 5 again. After 3 failed fixes, stop and report the error. |
   | `NO_CANVAS` | The script stopped before the renderer was created: check `three.min.js` is in the folder (`ls FILL_IN_FOLDER`), then run step 5 again. |
   | `NO_BROWSER` (or the command itself fails) | Skip the check; say "not checked in a browser" in step 6. |

6. **Report**: the full path of `index.html`; "double-click to open — no internet needed"; controls: left-drag rotates, scroll zooms, right-drag pans; and the step-5 result.

## Rules

- Offline always: never write `import`, `type="module"`, an import map, or a `<script src="https://...">`. Use the global `THREE` from `<script src="three.min.js"></script>`.
- The bundle contains three.js core plus ONLY `THREE.OrbitControls`, `THREE.CSS2DRenderer`, `THREE.CSS2DObject`. Other addons (GLTFLoader, FontLoader, TextGeometry, EffectComposer, …) are NOT in it. **ASK** before going online for one — options: (a) build it from the bundled pieces (e.g. `CSS2DObject` labels instead of `TextGeometry`), (b) load the addon from the internet, so the page then needs internet. Recommend (a).
- These APIs no longer exist — never write them: `THREE.Geometry`, `THREE.Face3` (use `BufferGeometry` + `setAttribute`), `THREE.CubeGeometry` (use `BoxGeometry`), `renderer.outputEncoding`, `renderer.useLegacyLights`.
- Lights use real-world units: keep the template's two lights. A `PointLight` needs about intensity 50 at 5 units away: `new THREE.PointLight(0xffffff, 50)`. An object that shows up black is not lit — switch its material to `MeshBasicMaterial` to confirm it exists, then fix the lights.
- Keep everything within -10..10 units on every axis (the camera at (8, 6, 10) frames that). Scale data into that range, as the bar chart does with `maxValue`.
- A double-clicked page cannot load files at runtime: no `fetch()`, no `TextureLoader` images, no model files. Use colours instead of textures and `data.js` (step 4) instead of `fetch()`. If the user insists on a texture or model file, the folder must be served: tell them to run `python3 -m http.server 8000` inside it and open http://localhost:8000/ — do not run the server yourself (it never exits).
- More than 500 data rows: never one `Mesh` per row — use one `THREE.Points` (step 3 scatter). More than 100,000 points: sample down first.
- Change the page with the `edit` tool. Never edit `three.min.js`. Never delete the error-recording `<script>` in `<head>` (step 5 depends on it).
- Step 5 is the whole check. Never write test scripts or harnesses (no `.mjs`/`.py` checkers, no browser profiles) — not in `FILL_IN_FOLDER`, not in `/tmp`.
- Flat 2D charts (bar/line/scatter on a page) are not three.js work: call the `skill` tool with `{"name": "d3js"}` instead.
