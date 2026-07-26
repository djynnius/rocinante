---
name: flask
description: "Build and debug Flask web apps and JSON APIs: app factory, blueprints, request handling, error handlers, config, testing. Use when asked to create a Flask app or endpoint, build a small Python web API, fix a Flask error, or test Flask routes."
---

# Flask

Small Python web apps and APIs. Run with the `bash` tool; install with `python3 -m pip install flask`. Always use the app-factory + blueprint shape below — it prevents the circular imports that break Flask projects.

1. **Skeleton** (two files):

   `app/__init__.py`:
   ```python
   from flask import Flask

   def create_app(config=None):
       app = Flask(__name__)
       app.config.from_prefixed_env()          # FLASK_* env vars become config
       if config:
           app.config.update(config)
       from app.routes import bp
       app.register_blueprint(bp)
       return app
   ```

   `app/routes.py`:
   ```python
   from flask import Blueprint, jsonify, request

   bp = Blueprint("api", __name__, url_prefix="/api")

   @bp.get("/items")
   def list_items():
       limit = request.args.get("limit", default=20, type=int)   # query string, typed
       return jsonify(items=[], limit=limit)

   @bp.post("/items")
   def create_item():
       data = request.get_json(silent=True)
       if not data or "name" not in data:
           return jsonify(error="name is required"), 400
       return jsonify(id=1, name=data["name"]), 201
   ```

2. **Run the dev server:**
```bash
flask --app app run --debug --port 5000
# if `flask` is not on PATH:
python3 -m flask --app app run --debug --port 5000
```
   Verify with `curl -s localhost:5000/api/items`. "Address already in use" → change `--port`.

3. **Request data — pick the right accessor:**
   | Where the data is | Accessor |
   |---|---|
   | Query string `?x=1` | `request.args.get("x", type=int)` |
   | Form post | `request.form.get("x")` |
   | JSON body | `request.get_json(silent=True)` (returns None on bad JSON — check it) |
   | File upload | `request.files["f"]` |

4. **Errors as JSON** (APIs should never return HTML errors):
```python
@bp.errorhandler(404)
def not_found(e):
    return jsonify(error="not found"), 404

@bp.errorhandler(Exception)
def boom(e):
    return jsonify(error=str(e)), 500
```

5. **Test with the built-in client** — no server needed:
```python
def test_create_item():
    app = create_app({"TESTING": True})
    client = app.test_client()
    r = client.post("/api/items", json={"name": "x"})
    assert r.status_code == 201
    assert r.get_json()["name"] == "x"
```
   Run with `python3 -m pytest`.

## Rules

- Return `(jsonify(...), status_code)` tuples; never bare dict + print debugging.
- Never ship `--debug`/`debug=True` beyond local dev (it exposes a code-execution console). Production runs behind gunicorn: `gunicorn "app:create_app()"`.
- Circular import error: something imports `app` at module top-level — move the import inside `create_app` (as the skeleton does) or import the blueprint module late.
- 404 on a route you just wrote: check the blueprint's `url_prefix` — the full path is prefix + route.
- Database layer: call the `skill` tool with `{"name": "sqlalchemy"}`; keep session-per-request (create in the route or use a teardown hook, do not share one global session).
