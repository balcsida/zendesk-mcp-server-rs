#!/usr/bin/env -S uv run
# /// script
# requires-python = ">=3.11"
# dependencies = ["pyyaml"]
# ///
"""Generate crates/zendesk/src/catalog.json: every Zendesk API operation, as data.

Inputs are Zendesk's OpenAPI specs and the collections of its public Postman workspace
"Zendesk Public API" (https://www.postman.com/zendesk-redback/workspace/zendesk-public-api).

    uv run scripts/gen_catalog.py                  # download the inputs
    uv run scripts/gen_catalog.py --specs DIR      # read support.yaml, help_center.yaml,
                                                   # voice.yaml and postman/<uid>.json from DIR
"""

import argparse
import contextlib
import json
import re
import sys
import time
import urllib.request
from collections import Counter
from pathlib import Path

import yaml

OUT = Path(__file__).resolve().parent.parent / "crates/zendesk/src/catalog.json"

# In precedence order: when two sources document the same operation, the first one wins.
SPECS = [
    ("support", "https://developer.zendesk.com/zendesk/oas.yaml", "support.yaml"),
    ("help_center", "https://developer.zendesk.com/help_center/oas.yaml", "help_center.yaml"),
    ("talk", "https://developer.zendesk.com/voice/oas.yaml", "voice.yaml"),
]

# Postman collections, in precedence order: (uid, title, group). A group of None is
# derived from each operation's path (see postman_group).
COLLECTIONS = [
    ("19931619-557586b7-9791-48c1-bc2a-81c6bcb90ba4", "Webhook API", "Webhooks"),
    ("19931619-6bc837c0-915b-446a-b8de-4399c5ff3f76", "Schedules API", "Schedules"),
    ("19931619-0fbc6ccf-4b85-4eba-84f9-853184e0ff4c", "Collaboration API", "Side Conversations"),
    ("19637986-6f2f6765-5cda-42d4-b46d-ef9137f12a6e", "Sunshine Profiles API", "Profiles"),
    ("19931619-678e0310-0069-43b1-a7af-6c9414c45f48", "Sunshine Events API", "Events"),
    ("19931619-ff70cc2e-6288-45d6-85f2-dc07f3600f0c", "Agent Availabilities API", "Agent Availabilities"),
    ("19931619-477c0e28-e54c-4c2b-90f8-4faf0e172509", "Unified Agent Status API", "Agent Statuses"),
    ("19931619-9163ecd6-4df1-4065-b319-6ca5b434ec1d", "Agent State Management API", "Agent Statuses"),
    ("53126082-7e04c068-591e-485d-b687-ec4a469d474b", "Omnichannel Engagements and Queue Reporting events", "Engagements"),
    ("19644669-ba2a771a-3a8e-4f87-9f15-6d6219eabf4e", "Omnichannel Engagements", "Engagements"),
    ("19637986-6f001f22-af9f-4311-b0bc-7b6889daf9fd", "Capacity Rules API", "Capacity Rules"),
    ("19931619-279bf0e0-8d39-45d2-be26-f6848e298f97", "Guide External Content API", "External Content"),
    ("19931619-c8ba23fd-94d7-4737-8ca6-1743b4952531", "Guide REST API", None),
    ("19931619-14a7c1e4-0590-4e11-a6f1-f2faa53d4205", "Guide JWT API", "Guide JWT"),
    ("19931619-fa010bae-22c3-4014-a36f-b7a9b37f6427", "AnswerBot API", "Answer Bot"),
    ("19637986-6d857e3e-0739-4a15-92fd-60f850b9b260", "Access Logs API", "Access Logs"),
    ("19931619-c84a51a4-6f0f-49b1-9908-9ce623708677", "Zendesk Application Market API", "Apps"),
    ("19931619-3c845a75-ae99-4b13-92bc-1f06d6776e34", "Jira Integration API", "Jira"),
    ("19931619-fa5d3273-5336-4805-b08f-4862e3052027", "ZIS Configurations API", "ZIS Configs"),
    ("19931619-b93e054f-8d74-4878-a302-a42c8fe06116", "ZIS Connections", "ZIS Connections"),
    ("19931619-8b2b19cf-4cf9-43da-91a9-f270daeaad46", "ZIS Inbound Webhooks API", "ZIS Inbound Webhooks"),
    ("19931619-405d6169-55ab-4972-bdbc-43be21943625", "ZIS Links API", "ZIS Links"),
    ("19931619-71d7fa33-21a7-4258-97bf-b8de58ded5a2", "ZIS Registry", "ZIS Registry"),
    ("19931619-af76de84-f8ba-4cd6-afc5-bb151f2e7c5f", "Chat Public API", None),
]

# Left out on purpose: (collection, reason).
EXCLUDED = [
    ("Custom Objects API", "legacy /api/sunshine custom objects, deprecated; replaced by Custom Objects in the Support spec"),
    ("Legacy Custom Objects API", "legacy /api/sunshine custom objects, deprecated; replaced by Custom Objects in the Support spec"),
    ("Status API", "other host, no auth"),
    ("Zendesk Public IPs API", "no auth"),
    ("Zendesk Attachments Service API", "other host"),
    ("Zendesk QA Public Import/Export API", "separate API tokens"),
    ("WFM API V2", "separate API tokens"),
    ("Account Service API", "private or internal"),
    ("Central Admin API", "private or internal"),
    ("Admin Center Framework Team Management API", "private or internal"),
    ("Explore Rails API", "private or internal"),
    ("Actor Management System API", "private or internal"),
    ("Platform Logs API", "private or internal"),
    ("Unleash Public API", "private or internal"),
]

# Guide REST API path prefix -> group, first match wins. Its test endpoints are skipped.
GUIDE_GROUPS = [
    ("/api/v2/gather/", "Badges"),
    ("/api/v2/guide/content_tags", "Content Tags"),
    ("/api/v2/guide/medias", "Guide Media"),
    ("/api/v2/guide/redirect_rules", "Redirect Rules"),
    ("/api/v2/guide/theming", "Themes"),
    ("/api/v2/guide/search", "Guide Search"),
    ("/api/v2/guide/user_images", "Guide User Images"),
    ("/api/v2/reporting/queue_", "Queue Reporting"),
    ("/api/v2/guide/external_content", "External Content"),
]
GUIDE_TEST_ENDPOINTS = ("coin_flip", "coin_flips", "hello_reporting")

# Parameters sent once per value (`explode`) that the specs mark only in prose.
REPEATED_PARAMS = {("ListAuditLogs", "filter[created_at]"), ("ExportAuditLogs", "filter[created_at]")}

CHAT_WORDS = {"oauth": "OAuth", "incremental": "Incremental Exports", "ip": "IP"}

METHODS = ["GET", "POST", "PUT", "PATCH", "DELETE"]
POSTMAN_TYPES = {"integer": "integer", "long": "integer", "number": "number",
                 "double": "number", "boolean": "boolean"}


# ---------------------------------------------------------------- inputs


def spec_loader():
    class Loader(yaml.SafeLoader):
        pass

    def as_text(loader, node):
        return loader.construct_scalar(node)

    # The Support YAML has a bare `=` scalar; dates stay as their source text.
    Loader.add_constructor("tag:yaml.org,2002:value", as_text)
    Loader.add_constructor("tag:yaml.org,2002:timestamp", as_text)
    return Loader


def fetch(url):
    for _ in range(10):
        request = urllib.request.Request(url, headers={"User-Agent": "gen_catalog"})  # noqa: S310
        with urllib.request.urlopen(request) as r:  # noqa: S310 - fixed https URLs only
            data = r.read()
        if b'"rate limited"' not in data[:200]:
            return data
        print(f"rate limited, retrying {url}", file=sys.stderr)
        time.sleep(15)
    sys.exit(f"still rate limited: {url}")


def load_spec(specs_dir, url, filename):
    data = (specs_dir / filename).read_bytes() if specs_dir else fetch(url)
    return yaml.load(data, Loader=spec_loader())  # noqa: S506 - a SafeLoader subclass


def load_collection(specs_dir, uid, first):
    if specs_dir:
        return json.loads((specs_dir / "postman" / f"{uid}.json").read_bytes())
    if not first:
        time.sleep(2)
    return json.loads(fetch(f"https://www.postman.com/collections/{uid}"))


# ---------------------------------------------------------------- text


def normalize(text, cap):
    text = "\n".join(line.rstrip() for line in (text or "").strip().split("\n"))
    text = re.sub(r"\n{3,}", "\n\n", text)
    text = re.sub(r"\]\(/(api-reference|documentation)/", r"](https://developer.zendesk.com/\1/", text)
    if len(text) > cap:
        head = text[:cap]
        cut = head.rfind("\n\n")
        text = (head[:cut] if cut > cap / 2 else head).rstrip() + " …"
    return text


def description(text):
    return normalize(text, 1000)


def pascal_case(name):
    # Asides such as "(i.e. unassigns a badge from a user)" stay in the summary only.
    name = re.sub(r"\((?!deprecated\))[^)]*\)", "", name)
    words = re.findall(r"[A-Za-z0-9]+", re.sub(r"['’]", "", name))
    return "".join(w[0].upper() + w[1:] for w in words)


def path_key(path):
    return re.sub(r"\{[^}]*\}", "{}", path)


# ---------------------------------------------------------------- OpenAPI


def resolve(root, node):
    while isinstance(node, dict) and "$ref" in node:
        target = root
        for part in node["$ref"].removeprefix("#/").split("/"):
            target = target[part.replace("~1", "/").replace("~0", "~")]
        node = target
    return node


def enum_text(value):
    return value if isinstance(value, str) else json.dumps(value)


def spec_param(root, raw):
    p = resolve(root, raw)
    schema = resolve(root, p.get("schema") or {})
    out = {"name": p["name"], "in": p["in"]}
    if p["in"] == "path" or p.get("required"):
        out["required"] = True
    if schema.get("type"):
        out["type"] = schema["type"]
    if desc := normalize(p.get("description"), 300):
        out["description"] = desc
    if schema.get("enum"):
        out["enum"] = [enum_text(v) for v in schema["enum"]]
    # `[]` names are always repeated; others only when the spec says so.
    if p.get("explode") and p.get("style", "form") == "form" and not p["name"].endswith("[]"):
        out["explode"] = True
    return out


def spec_body(root, op):
    rb = resolve(root, op.get("requestBody"))
    if not rb:
        return None
    content = rb.get("content") or {}
    content_type = "application/json" if "application/json" in content else next(iter(content), None)
    media = content.get(content_type) or {}
    body = {}
    if rb.get("required"):
        body["required"] = True
    if content_type and content_type != "application/json":
        body["content_type"] = content_type
    examples = media.get("examples")
    if examples:
        chosen = resolve(root, examples.get("default", next(iter(examples.values()))))
        if isinstance(chosen, dict) and "value" in chosen:
            body["example"] = chosen["value"]
    elif "example" in media:
        body["example"] = media["example"]
    return body


def spec_operations(root):
    for path, item in root["paths"].items():
        for method in METHODS:
            op = item.get(method.lower())
            if op is None:
                continue
            params = {}
            for raw in (item.get("parameters") or []) + (op.get("parameters") or []):
                p = spec_param(root, raw)
                if (op["operationId"], p["name"]) in REPEATED_PARAMS:
                    p["explode"] = True
                if p["in"] in ("path", "query"):
                    params[p["name"]] = p
            summary = op.get("summary") or ""
            yield {
                "id": op["operationId"],
                "tag": (op.get("tags") or [""])[0],
                "method": method,
                "path": path,
                "summary": summary + (" (deprecated)" if op.get("deprecated") else ""),
                "description": description(op.get("description")),
                "params": list(params.values()),
                "body": spec_body(root, op),
            }


# ---------------------------------------------------------------- Postman


def postman_items(items):
    for item in items:
        if "item" in item:
            yield from postman_items(item["item"])
        else:
            yield item


def postman_group(title, path):
    if title == "Guide REST API":
        for prefix, group in GUIDE_GROUPS:
            if path.startswith(prefix):
                return group
        if re.match(r"/api/v2/guide/(\{locale\}/)?survey", path):
            return "Surveys"
        sys.exit(f"{title}: no group for {path}")
    if not path.startswith("/api/v2/chat/"):
        sys.exit(f"{title}: unexpected path {path}")
    segment = path.removeprefix("/api/v2/chat/").split("/")[0]
    words = [CHAT_WORDS[w] if w in CHAT_WORDS else w.title() for w in segment.split("_")]
    return "Chat " + " ".join(words)


def postman_param(var, location):
    value = var.get("value") or ""
    text = var.get("description") or ""
    if isinstance(text, dict):
        text = text.get("content", "")
    out = {"name": var["key"], "in": location}
    if location == "path" or "(Required)" in text:
        out["required"] = True
    text = text.replace("(Required)", "")
    if m := re.fullmatch(r"<(\w+)(\[\])?>", value.strip()):
        out["type"] = POSTMAN_TYPES.get(m[1], "string")
    enum = re.search(r"\(This can only be one of ([^)]*)\)", text)
    if enum:
        out["enum"] = [v.strip() for v in enum[1].split(",") if v.strip()]
        text = text.replace(enum[0], "")
    if desc := normalize(text, 300):
        out["description"] = desc
    return out


def postman_body(request):
    body = request.get("body")
    if not body:
        return None
    if body["mode"] == "formdata":
        return {"content_type": "multipart/form-data"}
    if body["mode"] != "raw":
        sys.exit(f"unsupported Postman body mode {body['mode']!r}")
    if not (body.get("raw") or "").strip():
        return None
    out = {}
    headers = {h["key"].lower(): h["value"] for h in request.get("header", []) if not h.get("disabled")}
    content_type = headers.get("content-type", "application/json").split(";")[0].strip()
    if content_type != "application/json":
        out["content_type"] = content_type
    # A body that is not valid JSON gets no example.
    with contextlib.suppress(ValueError):
        out["example"] = json.loads(body["raw"])
    return out


def postman_operations(collection, title, group):
    for item in postman_items(collection["item"]):
        request = item["request"]
        url = request["url"]
        raw = url if isinstance(url, str) else url["raw"]
        url = {} if isinstance(url, str) else url
        path = re.sub(r"^(\{\{baseUrl\}\}|https://\{\{subdomain\}\}\.\{\{domain\}\}\.com)", "", raw)
        path = re.sub(r"\?.*", "", path)
        path = re.sub(r"(?<=/):(\w+)", r"{\1}", path)
        if not path.startswith("/"):
            sys.exit(f"{title}: {item['name']}: unexpected url {raw}")
        if title == "Guide REST API" and any(seg in GUIDE_TEST_ENDPOINTS for seg in path.split("/")):
            continue
        in_path = re.findall(r"\{([^}]*)\}", path)
        params = {}
        for var in url.get("variable") or []:
            if var["key"] in in_path:
                params[var["key"]] = postman_param(var, "path")
        for name in in_path:
            params.setdefault(name, {"name": name, "in": "path", "required": True})
        for q in url.get("query") or []:
            if not q.get("disabled") and q.get("key") and q["key"] not in params:
                params[q["key"]] = postman_param(q, "query")
        desc = request.get("description") or ""
        if isinstance(desc, dict):
            desc = desc.get("content", "")
        yield {
            "id": pascal_case(item["name"]),
            "group": group or postman_group(title, path),
            "method": request["method"].upper(),
            "path": path,
            "summary": item["name"].strip().removesuffix("."),
            "description": description(desc),
            "params": list(params.values()),
            "body": postman_body(request),
        }


# ---------------------------------------------------------------- catalog


def spec_group(source, tag, support_tags):
    if source == "help_center":
        if tag.lower() in support_tags and not tag.startswith("Help Center"):
            return "Help Center " + tag
        return tag
    if source == "talk":
        return tag if tag.startswith("Talk") else "Talk " + tag
    return tag


def build(specs_dir):
    taken = set()  # (method, path with placeholders normalized)
    ids = set()
    ops = []
    per_source = Counter()
    skipped = Counter()
    support_tags = set()

    def add(source, op, id_prefix):
        key = (op["method"], path_key(op["path"]))
        if key in taken:
            skipped[source] += 1
            return
        taken.add(key)
        if op["id"].lower() in ids:
            op["id"] = id_prefix + op["id"]
            if op["id"].lower() in ids:
                sys.exit(f"id collision that a prefix does not resolve: {op['id']}")
        ids.add(op["id"].lower())
        ops.append(op)
        per_source[source] += 1

    for name, url, filename in SPECS:
        root = load_spec(specs_dir, url, filename)
        if name == "support":
            support_tags = {t["name"].lower() for t in root.get("tags", [])}
        for op in spec_operations(root):
            op["group"] = spec_group(name, op.pop("tag"), support_tags)
            add(name, op, {"help_center": "HelpCenter", "talk": "Talk"}.get(name, ""))

    for n, (uid, title, group) in enumerate(COLLECTIONS):
        collection = load_collection(specs_dir, uid, n == 0)
        if collection["info"]["name"] != title:
            sys.exit(f"{uid} is {collection['info']['name']!r}, expected {title!r}")
        for op in postman_operations(collection, title, group):
            # Chat groups are "Chat Roles" but their prefix is just "Chat": ChatListRoles.
            prefix = "Chat" if title == "Chat Public API" else op["group"].replace(" ", "")
            add(f"postman: {title}", op, prefix)

    ops.sort(key=lambda o: (o["group"].lower(), o["path"], METHODS.index(o["method"]), o["id"]))
    return ops, per_source, skipped


def compact(op):
    out = {"id": op["id"], "group": op["group"], "method": op["method"], "path": op["path"]}
    out["summary"] = op["summary"]
    if op["description"]:
        out["description"] = op["description"]
    if op["params"]:
        out["params"] = op["params"]
    if op["body"] is not None:
        out["body"] = op["body"]
    return out


def dumps(value):
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"))


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawTextHelpFormatter)
    parser.add_argument("--specs", type=Path, help="read the inputs from this directory instead of downloading")
    args = parser.parse_args()

    ops, per_source, skipped = build(args.specs)
    lines = ",\n".join(dumps(compact(op)) for op in ops)
    OUT.write_text(f"[\n{lines}\n]\n", encoding="utf-8")

    print(f"wrote {len(ops)} operations to {OUT} ({OUT.stat().st_size:,} bytes)")
    print("operations per source:")
    for source, count in per_source.items():
        print(f"  {source}: {count}")
    print(f"duplicates skipped: {sum(skipped.values())}")
    for source, count in skipped.items():
        print(f"  {source}: {count}")
    print("collections excluded:")
    for title, reason in EXCLUDED:
        print(f"  {title}: {reason}")


if __name__ == "__main__":
    main()
