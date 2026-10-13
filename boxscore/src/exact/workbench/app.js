// boxscore workbench: a view over the server's /v1/calc routes: list and review revisions,
// load a period from files the user chooses, look up a trace.
//
// Rules this file keeps (a cargo test checks the mechanical ones):
// - It never computes a figure. Every number shown is a string from an
//   envelope; thousands separators are inserted into the integer part by
//   string manipulation for display only, and the raw string stays in the
//   cell's title.
// - Every value from the server is untrusted text. The DOM is built with
//   createElement and textContent only; nothing is parsed as markup.
// - The API key lives in sessionStorage only and is sent as a bearer token.
(function () {
  "use strict";

  var TIMEOUT_MS = 15000;
  // An import reads, parses and stores up to 2 MiB, so it waits longer.
  var IMPORT_TIMEOUT_MS = 60000;
  var KEY_STORAGE = "boxscore.workbench.apiKey";
  // The server's cap on one request body, in bytes (2 MiB): the files of an
  // import must fit in it as JSON, escaping included.
  var MAX_FILE_BYTES = 2097152;
  // Bounds on the recorded files table and on any raw JSON the page shows.
  var MAX_FILE_ROWS = 50;
  var MAX_RAW_CHARS = 65536;

  // The engine's account classes (boxscore/src/ontology.rs account_class,
  // documented in START_HERE.md "Loading your own export"): these categories
  // are revenue, "unmapped" is kept out of NOI, anything else is an expense.
  // A cargo test checks this list against the engine.
  var REVENUE_CATEGORIES = ["rental income", "concessions", "bad debt", "other income"];

  // One-line meanings, from START_HERE.md "What review_required means".
  var REASONS = {
    missing_actuals: "One side of the month has no rows: there are no actual rows.",
    missing_budgets: "One side of the month has no rows: there are no budget rows.",
    missing_occupancy_snapshot: "No occupancy snapshot was imported.",
    incomplete_account_coverage: "An account has an actual but no budget, or the reverse; it is left out of the NOI bridge and listed.",
    unmapped_accounts: "A line is marked unmapped, so it is not in NOI.",
    negative_net_expenses: "Expenses add up below zero, usually a sign problem in the export.",
    missing_periods: "A twelve-month statement found a month with nothing loaded; it is shown as empty, never as zero.",
    down_units_present: "Some units are offline. Information only."
  };

  var STATUS_TEXT = {
    calculated: "calculated: done, nothing flagged",
    review_required: "review_required: done, and a person should look before relying on it",
    refused: "refused: the engine did not compute"
  };

  var MONEY = /^-?[0-9]+(\.[0-9]+)?$/;
  var ZERO = /^-?0+(\.0+)?$/;
  // An API key is printable ASCII with no spaces; anything else (a pasted
  // zero-width space, say) would make fetch throw on the header.
  var KEY_CHARS = /^[\x21-\x7e]+$/;

  // selected: the revision under review. reviewSeq, listSeq, importSeq and
  // traceSeq number each request a loader starts; only the latest one's
  // response is rendered, so overlapping requests (a double click, a second
  // Reload) render once. known: the listed revisions by id. adapters: the
  // server's adapter specs; fields: the form inputs built from the chosen
  // one (DOM nodes only: file contents are read when sending and live in
  // that request alone). confirm: a correction awaiting its confirmation.
  var state = {
    selected: null, reviewSeq: 0, listSeq: 0, importSeq: 0, traceSeq: 0,
    known: {}, adapters: [], adaptersKey: "", fields: { files: [], options: [] },
    confirm: null, busy: false
  };

  function has(object, key) {
    return Object.prototype.hasOwnProperty.call(object, key);
  }

  // ---------------------------------------------------------------- DOM

  function $(id) {
    return document.getElementById(id);
  }

  function el(tag, attrs, children) {
    var node = document.createElement(tag);
    if (attrs) {
      Object.keys(attrs).forEach(function (name) {
        var value = attrs[name];
        if (value === null || value === undefined || value === false) return;
        if (name === "className") node.className = value;
        else if (name === "text") node.textContent = String(value);
        else node.setAttribute(name, value === true ? "" : String(value));
      });
    }
    (children || []).forEach(function (child) {
      if (child === null || child === undefined) return;
      node.appendChild(typeof child === "string" ? document.createTextNode(child) : child);
    });
    return node;
  }

  function clear(node) {
    while (node.firstChild) node.removeChild(node.firstChild);
    return node;
  }

  function setStatus(message, isError) {
    var node = $("status");
    node.textContent = message;
    node.className = isError ? "status error" : "status";
  }

  // Text for any value: strings as they are, anything absent said so.
  function text(value) {
    if (typeof value === "string") return value;
    if (value === null || value === undefined) return "(not returned)";
    if (typeof value === "number" || typeof value === "boolean") return String(value);
    return JSON.stringify(value);
  }

  function shortId(id) {
    var s = text(id);
    return s.length > 8 ? s.slice(0, 8) + "…" : s;
  }

  // Thousands separators in the integer part, by string manipulation only.
  function money(value) {
    var raw = text(value);
    if (typeof value !== "string" || !MONEY.test(value)) return raw;
    var negative = value.charAt(0) === "-";
    var body = negative ? value.slice(1) : value;
    var dot = body.indexOf(".");
    var whole = dot < 0 ? body : body.slice(0, dot);
    var rest = dot < 0 ? "" : body.slice(dot);
    var grouped = "";
    while (whole.length > 3) {
      grouped = "," + whole.slice(-3) + grouped;
      whole = whole.slice(0, -3);
    }
    return (negative ? "-" : "") + whole + grouped + rest;
  }

  function moneyCell(value) {
    return el("td", { className: "num", title: text(value), text: money(value) });
  }

  function table(caption, headers, rows, numeric) {
    var head = el("tr", null, headers.map(function (h, i) {
      return el("th", { scope: "col", className: numeric && numeric.indexOf(i) >= 0 ? "num" : null, text: h });
    }));
    var body = el("tbody", null, rows);
    var tableNode = el("table", null, [caption ? el("caption", { text: caption }) : null, el("thead", null, [head]), body]);
    return el("div", { className: "scroll" }, [tableNode]);
  }

  function errorBlock(lines) {
    return el("div", { className: "error-block", role: "alert" }, lines.map(function (line) {
      return el("p", { text: line });
    }));
  }

  // A definition list from [label, value, className] triples.
  function dl(pairs) {
    var kids = [];
    pairs.forEach(function (p) {
      kids.push(el("dt", { text: p[0] }), el("dd", { className: p[2] || null, text: p[1] }));
    });
    return el("dl", { className: "meta" }, kids);
  }

  // A value the server returned as null is said to be none; an absent one,
  // not returned.
  function orNone(value) {
    return value === null ? "none" : text(value);
  }

  function hashCell(value, full) {
    var raw = text(value);
    return el("td", { className: "mono wrap-text", title: raw, text: full || raw.length <= 12 ? raw : raw.slice(0, 12) + "…" });
  }

  function statusLine(status) {
    status = text(status);
    return el("p", null, [
      el("span", { className: "badge " + (has(STATUS_TEXT, status) ? status : "refused"), text: status }),
      " ",
      el("span", { className: "muted", text: has(STATUS_TEXT, status) ? STATUS_TEXT[status] : "" })
    ]);
  }

  // A value as indented JSON in a <details>, cut at MAX_RAW_CHARS.
  function rawDetails(summary, value) {
    var raw = JSON.stringify(value, null, 2) || "";
    var cut = raw.length > MAX_RAW_CHARS;
    return el("details", null, [
      el("summary", { text: summary }),
      el("pre", { className: "raw", text: cut ? raw.slice(0, MAX_RAW_CHARS) : raw }),
      cut ? el("p", { className: "note", text: "Truncated: the first " + MAX_RAW_CHARS + " of " + raw.length + " characters." }) : null
    ]);
  }

  // The recorded sources of a revision. profile and files are written only
  // by the server; supplied is the caller's own and is shown apart, as such.
  // With full, whole hashes and the whole object; otherwise shortened hashes.
  function sourcesBlock(sources, full) {
    if (!sources || typeof sources !== "object" || Array.isArray(sources)) {
      return [el("p", { className: "note", text: "No sources are recorded for this revision." })];
    }
    var nodes = [];
    if (has(sources, "profile")) nodes.push(dl([["Profile", text(sources.profile), "mono"]]));
    var files = Array.isArray(sources.files) ? sources.files : [];
    if (files.length) {
      nodes.push(table("Recorded files", ["Name", "Bytes", "SHA-256"], files.slice(0, MAX_FILE_ROWS).map(function (f) {
        f = f || {};
        return el("tr", null, [
          el("td", { className: "wrap-text", text: text(f.name) }),
          el("td", { className: "num", text: text(f.bytes) }),
          hashCell(f.sha256, full)
        ]);
      }), [1]));
      if (files.length > MAX_FILE_ROWS) nodes.push(el("p", { className: "note", text: (files.length - MAX_FILE_ROWS) + " more" }));
    } else {
      nodes.push(el("p", { className: "note", text: "The record lists no files." }));
    }
    if (has(sources, "supplied")) nodes.push(rawDetails("Supplied by the caller (not verified by the server)", sources.supplied));
    if (full) nodes.push(rawDetails("All recorded sources", sources));
    return nodes;
  }

  // The envelope's citation line with Copy and, when it has a trace id and
  // lookup is set, a Look up button that fills the trace screen.
  function citationRow(line, traceId, id, lookup) {
    var code = el("code", { id: id, text: line });
    var copy = el("button", { type: "button", text: "Copy" });
    copy.addEventListener("click", function () { copyText(code, line); });
    var kids = [code, copy];
    if (lookup && typeof traceId === "string" && traceId) {
      var find = el("button", { type: "button", text: "Look up", "aria-label": "Look up trace " + traceId });
      find.addEventListener("click", function () { lookupTrace(traceId); });
      kids.push(find);
    }
    return el("div", { className: "citation" }, kids);
  }

  function citationOf(envelope) {
    return typeof envelope.citation === "string" ? envelope.citation : citationFrom(envelope);
  }

  // ---------------------------------------------------------------- key

  function getKey() {
    try {
      return window.sessionStorage.getItem(KEY_STORAGE) || "";
    } catch (e) {
      return "";
    }
  }

  function setKey(value) {
    try {
      if (value) window.sessionStorage.setItem(KEY_STORAGE, value);
      else window.sessionStorage.removeItem(KEY_STORAGE);
    } catch (e) {
      setStatus("This browser refused session storage; the key cannot be kept.", true);
    }
  }

  function showKeyPanel() {
    $("key-panel").hidden = false;
  }

  // ---------------------------------------------------------------- HTTP

  // Resolves to { status, body, retryAfter } or { failure, timedOut } and
  // never throws.
  function request(method, path, payload, timeout) {
    timeout = timeout || TIMEOUT_MS;
    var controller = new AbortController();
    var timer = window.setTimeout(function () { controller.abort(); }, timeout);
    var headers = { Accept: "application/json" };
    if (payload !== undefined) headers["Content-Type"] = "application/json";
    var key = getKey();
    if (key) headers.Authorization = "Bearer " + key;
    return window.fetch(path, {
      method: method,
      headers: headers,
      body: payload === undefined ? undefined : JSON.stringify(payload),
      signal: controller.signal,
      cache: "no-store",
      credentials: "omit",
      referrerPolicy: "no-referrer",
      redirect: "error"
    }).then(function (response) {
      return response.text().then(function (raw) {
        var body = null;
        try {
          body = raw ? JSON.parse(raw) : null;
        } catch (e) {
          body = null;
        }
        return { status: response.status, body: body, retryAfter: response.headers.get("Retry-After") };
      });
    }, function (error) {
      var timedOut = !!error && error.name === "AbortError";
      return {
        timedOut: timedOut,
        failure: timedOut
          ? "No answer from the server within " + timeout / 1000 + " seconds."
          : "Network failure: the server could not be reached."
      };
    }).finally(function () {
      window.clearTimeout(timer);
    });
  }

  function calc(name, input, timeout) {
    // Relative, so a server behind a path prefix (a public URL ending in
    // /boxscore, say) is called under that prefix: the page is served at a
    // directory URL ending in a slash.
    return request("POST", "v1/calc/" + name, input, timeout);
  }

  // Is this response an envelope (200, or 422 refused)?
  function isEnvelope(res) {
    return !res.failure && (res.status === 200 || res.status === 422) && res.body && typeof res.body.status === "string";
  }

  // Lines describing a transport failure: status, code, message, Retry-After.
  function describeFailure(res) {
    if (res.failure) return [res.failure];
    var lines = ["HTTP " + res.status];
    var error = res.body && res.body.error;
    if (error && typeof error === "object") {
      lines[0] += " " + text(error.code);
      lines.push(text(error.message));
    } else if (!res.body) {
      lines.push("The response was not JSON.");
    }
    if (res.status === 401) {
      lines.push(getKey() ? "The API key was not accepted. Paste another below." : "This server needs an API key. Paste it below.");
      showKeyPanel();
    }
    if (res.status === 429) {
      lines.push("Rate limited: retry after " + (res.retryAfter ? res.retryAfter + " seconds" : "a short wait") + ".");
    }
    if (res.status === 503 && res.retryAfter) {
      lines.push("Retry after " + res.retryAfter + " seconds.");
    }
    return lines;
  }

  function refusalBlock(envelope) {
    var error = envelope.error || {};
    return el("div", { className: "refusal", role: "alert" }, [
      el("p", null, [el("span", { className: "badge refused", text: "refused" }), " ", el("span", { className: "code", text: text(error.code) })]),
      el("p", { text: text(error.message) })
    ]);
  }

  // ---------------------------------------------------------------- screen 1

  function loadRevisions() {
    var seq = ++state.listSeq;
    setStatus("Loading the list of revisions…");
    return calc("list_revisions", {}).then(function (res) {
      if (seq !== state.listSeq) return;
      var target = clear($("revisions"));
      if (!isEnvelope(res)) {
        var lines = describeFailure(res);
        setStatus("Could not list revisions: " + lines.join(" · "), true);
        target.appendChild(errorBlock(lines));
        return;
      }
      var envelope = res.body;
      if (envelope.status === "refused") {
        setStatus("list_revisions was refused.", true);
        target.appendChild(refusalBlock(envelope));
        return;
      }
      var revisions = (envelope.result && envelope.result.revisions) || [];
      state.known = {};
      if (Array.isArray(revisions)) {
        revisions.forEach(function (r) {
          if (r && typeof r.id === "string") state.known[r.id] = r;
        });
      }
      if (!Array.isArray(revisions) || revisions.length === 0) {
        target.appendChild(el("p", { className: "note", text: "No revisions are stored in this server's database yet." }));
        setStatus("No revisions loaded.");
        return;
      }
      target.appendChild(revisionTable(revisions));
      target.appendChild(el("p", { className: "note", text: envelope.citation ? "Listed by " + envelope.citation : "" }));
      setStatus(revisions.length + (revisions.length === 1 ? " revision" : " revisions") + " loaded. Select one to review it.");
    });
  }

  function revisionTable(revisions) {
    var rows = revisions.map(function (rev) {
      var reports = Array.isArray(rev.reports) ? rev.reports : [];
      var reportIds = reports.map(function (r) { return text(r && r.id); });
      var correction = rev.parent_id
        ? "supersedes " + shortId(rev.parent_id) + (rev.reason ? ": " + text(rev.reason) : "")
        : "";
      var row = el("tr", {
        className: "pick",
        tabindex: "0",
        "data-revision": text(rev.id),
        "aria-label": "Review " + text(rev.property) + " " + text(rev.period) + " revision " + text(rev.revision)
      }, [
        el("td", { className: "wrap-text", text: text(rev.property) }),
        el("td", { text: text(rev.period) }),
        el("td", { className: "num", text: text(rev.revision) }),
        el("td", { className: "mono", title: text(rev.id), text: shortId(rev.id) }),
        el("td", { className: "num", text: text(rev.unit_count) }),
        el("td", { text: rev.created_at ? text(rev.created_at) : "" }),
        el("td", { className: "wrap-text", title: rev.parent_id ? text(rev.parent_id) : null, text: correction }),
        el("td", { title: reportIds.join("\n") || null, text: reports.length ? reports.length + " issued" : "none" })
      ]);
      function pick() { selectRevision(text(rev.id)); }
      row.addEventListener("click", pick);
      row.addEventListener("keydown", function (event) {
        if (event.key === "Enter" || event.key === " ") {
          event.preventDefault();
          pick();
        }
      });
      return row;
    });
    return table("Stored revisions. Select a row to review it.",
      ["Property", "Period", "Revision", "Revision id", "Units", "Imported at", "Correction", "Reports"],
      rows, [2, 4]);
  }

  function markSelected(id) {
    var rows = document.querySelectorAll("tr.pick");
    Array.prototype.forEach.call(rows, function (row) {
      if (row.getAttribute("data-revision") === id) row.setAttribute("aria-current", "true");
      else row.removeAttribute("aria-current");
    });
  }

  // ---------------------------------------------------------------- screen 2

  // note: a sentence to put before the outcome in the status line.
  function selectRevision(id, note) {
    note = note || "";
    id = id.trim();
    if (!id) {
      setStatus("Paste a revision id first.", true);
      return;
    }
    state.selected = id;
    prefillSupersedes();
    var seq = ++state.reviewSeq;
    markSelected(id);
    $("review").hidden = false;
    setStatus("Reviewing revision " + id + "…");
    calc("review_period", { revision_id: id }).then(function (res) {
      if (seq !== state.reviewSeq) return;
      var body = clear($("review-body"));
      if (!isEnvelope(res)) {
        var lines = describeFailure(res);
        setStatus(note + "Review failed: " + lines.join(" · "), true);
        body.appendChild(errorBlock(lines));
      } else {
        renderReview(body, res.body);
        setStatus(note + "Review of revision " + id + ": " + text(res.body.status) + ".", res.body.status === "refused");
      }
      $("review-h").focus();
    });
  }

  // Class parity with the engine: classify on the exact category string the
  // engine returned, with no trim and no case change. The engine has already
  // trimmed and lowercased it (exact/variance.rs), and Rust's trim keeps
  // U+FEFF while JavaScript's trim() strips it, so re-normalizing here would
  // call a category of "\uFEFFrental income" revenue while the engine counts
  // it as an expense. A cargo test pins the engine's side.
  function classOf(category) {
    var c = text(category);
    if (REVENUE_CATEGORIES.indexOf(c) >= 0) return "revenue";
    if (c === "unmapped") return "unmapped";
    return "expense";
  }

  // Every variance is actual minus budget (docs/GLOSSARY.md, NOI bridge).
  // Under the positive-costs convention a positive variance is favorable for
  // revenue and NOI and unfavorable for expenses. Only the sign of the
  // returned string is read; nothing is computed.
  function direction(kind, variance, convention) {
    if (convention !== "positive_costs" || kind === "unmapped") return null;
    if (typeof variance !== "string" || !MONEY.test(variance)) return null;
    if (ZERO.test(variance)) return { word: "on budget", className: "" };
    var positive = variance.charAt(0) !== "-";
    var favorable = kind === "expense" ? !positive : positive;
    return favorable
      ? { word: "▲ favorable", className: "fav" }
      : { word: "▼ unfavorable", className: "unfav" };
  }

  function varianceCells(kind, variance, convention) {
    var d = direction(kind, variance, convention);
    return [
      moneyCell(variance),
      el("td", { className: d ? d.className : "muted", text: d ? d.word : "—" })
    ];
  }

  function renderReview(body, envelope) {
    var result = envelope.result || {};
    var status = text(envelope.status);
    body.appendChild(el("p", null, [
      el("span", { className: "badge " + (has(STATUS_TEXT, status) ? status : "refused"), text: status }),
      " ",
      el("span", { className: "muted", text: has(STATUS_TEXT, status) ? STATUS_TEXT[status] : "" })
    ]));

    if (status === "refused") {
      body.appendChild(refusalBlock(envelope));
      appendProvenance(body, envelope);
      return;
    }

    var convention = result.expense_convention;
    body.appendChild(el("dl", { className: "meta" }, [
      el("dt", { text: "Property" }), el("dd", { text: text(result.property) }),
      el("dt", { text: "Period" }), el("dd", { text: text(result.period) }),
      el("dt", { text: "Revision id" }), el("dd", { className: "mono", text: text(result.revision_id) }),
      el("dt", { text: "Units" }), el("dd", { text: text(result.unit_count) }),
      el("dt", { text: "Convention" }), el("dd", { text: text(result.currency) + ", " + text(convention) }),
      el("dt", { text: "Human approval" }), el("dd", { text: text(result.human_approval) })
    ]));

    var variance = result.variance || {};
    var reasons = Array.isArray(variance.review_reasons) ? variance.review_reasons : [];
    body.appendChild(el("h3", { text: "Review reasons" }));
    if (reasons.length === 0) {
      body.appendChild(el("p", { className: "note", text: "None: nothing is flagged." }));
    } else {
      body.appendChild(el("ul", { className: "reasons" }, reasons.map(function (reason) {
        var code = text(reason);
        var meaning = has(REASONS, code)
          ? REASONS[code]
          : "No description on this page; see docs/GLOSSARY.md.";
        return el("li", null, [el("code", { text: code }), ": " + meaning]);
      })));
    }

    if (result.supersedes_revision) {
      body.appendChild(el("h3", { text: "Correction" }));
      body.appendChild(el("dl", { className: "meta" }, [
        el("dt", { text: "Supersedes" }), el("dd", { className: "mono", text: text(result.supersedes_revision) }),
        el("dt", { text: "Reason" }), el("dd", { text: text(result.correction_reason) })
      ]));
      var changes = result.changes && typeof result.changes === "object" ? result.changes : {};
      var fields = Object.keys(changes);
      if (fields.length) {
        body.appendChild(table("Change in each bridge value versus the superseded revision",
          ["Field", "Change"],
          fields.map(function (field) {
            return el("tr", null, [el("td", { className: "mono", text: field }), moneyCell(changes[field])]);
          }), [1]));
      }
    }

    var bridge = variance.noi_bridge || {};
    body.appendChild(el("h3", { text: "NOI bridge" }));
    var lines = [
      ["Revenue", "revenue", "actual_revenue", "budget_revenue", "revenue_variance"],
      ["Expenses", "expense", "actual_expenses", "budget_expenses", "expense_variance"],
      ["NOI", "noi", "actual_noi", "budget_noi", "noi_variance"]
    ];
    body.appendChild(table(null, ["", "Actual", "Budget", "Variance", "Direction"],
      lines.map(function (line) {
        return el("tr", null, [el("th", { scope: "row", text: line[0] }), moneyCell(bridge[line[2]]), moneyCell(bridge[line[3]])]
          .concat(varianceCells(line[1], bridge[line[4]], convention)));
      }), [1, 2, 3]));
    body.appendChild(el("p", { className: "note", text: "Variance is actual minus budget. Scope: " + text(result.bridge_scope) + " (accounts present in both actuals and budgets)." }));

    var accounts = Array.isArray(variance.by_account) ? variance.by_account : [];
    var unmapped = accounts.filter(function (a) { return classOf(a && a.category) === "unmapped"; });
    body.appendChild(el("h3", { text: "By account" }));
    var filter = el("input", { type: "search", id: "account-filter", autocomplete: "off", spellcheck: "false" });
    body.appendChild(el("div", { className: "inline-form" }, [el("label", { for: "account-filter", text: "Filter accounts" }), filter]));
    var accountRows = accounts.map(function (a) {
      a = a || {};
      var kind = classOf(a.category);
      var row = el("tr", null, [
        el("td", { className: "mono", text: text(a.account_code) }),
        el("td", { className: "wrap-text", text: text(a.account_name) }),
        el("td", { text: text(a.category) }),
        el("td", { text: kind }),
        moneyCell(a.actual),
        moneyCell(a.budget)
      ].concat(varianceCells(kind, a.variance, convention)));
      row.setAttribute("data-filter", [text(a.account_code), text(a.account_name), text(a.category), kind].join(" ").toLowerCase());
      return row;
    });
    body.appendChild(table(null, ["Code", "Name", "Category", "Class", "Actual", "Budget", "Variance", "Direction"], accountRows, [4, 5, 6]));
    body.appendChild(el("p", { className: "note", text: "Class is read from the category by the engine's rule: rental income, concessions, bad debt and other income are revenue; unmapped is kept out of NOI; any other category is an expense." }));
    filter.addEventListener("input", function () {
      var wanted = filter.value.trim().toLowerCase();
      accountRows.forEach(function (row) {
        row.hidden = wanted !== "" && row.getAttribute("data-filter").indexOf(wanted) < 0;
      });
    });

    var excluded = Array.isArray(result.excluded_accounts) ? result.excluded_accounts : [];
    if (excluded.length) {
      body.appendChild(el("h3", { text: "Excluded accounts (incomplete coverage)" }));
      body.appendChild(table("Lines with no counterpart on the other side; not in the NOI bridge",
        ["Side", "Code", "Name", "Category", "Amount", "Reason"],
        excluded.map(function (x) {
          x = x || {};
          var line = x.line || {};
          return el("tr", null, [
            el("td", { text: text(x.kind) }),
            el("td", { className: "mono", text: text(line.account_code) }),
            el("td", { className: "wrap-text", text: text(line.account_name) }),
            el("td", { text: text(line.category) }),
            moneyCell(line.amount),
            el("td", { text: text(x.reason) })
          ]);
        }), [4]));
    }

    if (unmapped.length) {
      body.appendChild(el("h3", { text: "Unmapped accounts (not in NOI)" }));
      body.appendChild(table(null, ["Code", "Name", "Actual", "Budget"], unmapped.map(function (a) {
        return el("tr", null, [
          el("td", { className: "mono", text: text(a.account_code) }),
          el("td", { className: "wrap-text", text: text(a.account_name) }),
          moneyCell(a.actual),
          moneyCell(a.budget)
        ]);
      }), [2, 3]));
      body.appendChild(el("p", { className: "note" }, [
        "Unmapped totals: actual ",
        el("span", { title: text(bridge.unmapped_actual), text: money(bridge.unmapped_actual) }),
        ", budget ",
        el("span", { title: text(bridge.unmapped_budget), text: money(bridge.unmapped_budget) }),
        "."
      ]));
    }

    appendProvenance(body, envelope);
  }

  // The engine's citation line, then the whole envelope. opts: id of the
  // citation element, the heading, and whether it gets a Look up button.
  function appendProvenance(body, envelope, opts) {
    opts = opts || { id: "citation", heading: "Citation", lookup: true };
    body.appendChild(el("h3", { text: opts.heading }));
    body.appendChild(citationRow(citationOf(envelope), envelope.trace_id, opts.id, opts.lookup));
    body.appendChild(rawDetails("Raw envelope", envelope));
  }

  // Only when an envelope carries no citation: the engine's format, from its
  // own fields.
  function citationFrom(e) {
    var line = "boxscore " + text(e.function) + "@" + text(e.version) + " · trace " + text(e.trace_id) +
      " · input sha256:" + text(e.input_sha256).slice(0, 12) + "…";
    if (typeof e.output_sha256 === "string") line += " · output sha256:" + e.output_sha256.slice(0, 12) + "…";
    else line += " · refused" + (e.error && e.error.code ? " " + text(e.error.code) : "");
    if (e.result && typeof e.result.revision_id === "string") line += " · revision " + e.result.revision_id;
    return line;
  }

  function selectNode(node) {
    var range = document.createRange();
    range.selectNodeContents(node);
    var selection = window.getSelection();
    selection.removeAllRanges();
    selection.addRange(range);
  }

  function copyText(node, line) {
    function fallback() {
      selectNode(node);
      var copied = false;
      try {
        copied = document.execCommand("copy");
      } catch (e) {
        copied = false;
      }
      setStatus(copied ? "Citation copied." : "The citation is selected: press Ctrl+C (Cmd+C on a Mac) to copy it.");
    }
    if (navigator.clipboard && window.isSecureContext) {
      navigator.clipboard.writeText(line).then(function () { setStatus("Citation copied."); }, fallback);
    } else {
      fallback();
    }
  }

  // ---------------------------------------------------------------- import

  function currentAdapter() {
    var profile = $("import-profile").value;
    for (var i = 0; i < state.adapters.length; i++) {
      if (state.adapters[i].profile === profile) return state.adapters[i];
    }
    return null;
  }

  // The form comes from describe's adapter list: a profile choice, one file
  // input per file the adapter names and one text input per option.
  function setAdapters(result) {
    var list = result && Array.isArray(result.adapters)
      ? result.adapters.filter(function (a) { return a && typeof a.profile === "string"; })
      : [];
    var key = JSON.stringify(list);
    if (key === state.adaptersKey) return;
    state.adaptersKey = key;
    state.adapters = list;
    var select = clear($("import-profile"));
    list.forEach(function (a) { select.appendChild(el("option", { value: a.profile, text: a.profile })); });
    select.disabled = list.length === 0;
    renderFields();
  }

  function helpText(spec) {
    var parts = [];
    if (spec.description) parts.push(text(spec.description) + ".");
    if (spec.shape) parts.push("Shape: " + text(spec.shape) + ".");
    return parts.join(" ");
  }

  function renderFields() {
    var kept = {};
    state.fields.options.forEach(function (f) { kept[f.name] = f.input.value; });
    var host = clear($("import-fields"));
    state.fields = { files: [], options: [] };
    var adapter = currentAdapter();
    $("import-summary").textContent = adapter && adapter.summary ? text(adapter.summary) : "";
    if (!adapter) {
      host.appendChild(el("p", { className: "note", text: "The server listed no source adapters." }));
      return;
    }
    (Array.isArray(adapter.files) ? adapter.files : []).forEach(function (spec, i) {
      spec = spec || {};
      var id = "import-file-" + i;
      var input = el("input", { type: "file", id: id, "aria-describedby": id + "-help", "aria-required": spec.required ? "true" : null });
      host.appendChild(el("div", { className: "field" }, [
        el("label", { for: id, text: text(spec.name) + (spec.required ? " (required)" : " (optional)") }),
        input,
        el("p", { className: "note", id: id + "-help", text: helpText(spec) })
      ]));
      state.fields.files.push({ name: text(spec.name), required: !!spec.required, input: input });
    });
    (Array.isArray(adapter.options) ? adapter.options : []).forEach(function (spec, i) {
      spec = spec || {};
      var id = "import-opt-" + i;
      var name = text(spec.name);
      var input = el("input", { type: "text", id: id, autocomplete: "off", spellcheck: "false", "aria-describedby": id + "-help", "aria-required": spec.required ? "true" : null });
      if (has(kept, name)) input.value = kept[name];
      host.appendChild(el("div", { className: "field" }, [
        el("label", { for: id, text: name + (spec.required ? " (required)" : " (optional)") }),
        input,
        el("p", { className: "note", id: id + "-help", text: helpText(spec) })
      ]));
      state.fields.options.push({ name: name, required: !!spec.required, input: input });
    });
  }

  // Fill the revision to supersede from the one under review, unless the
  // person has typed their own or the form is locked for a confirmation.
  function prefillSupersedes() {
    var input = $("import-supersedes");
    if (!state.selected || $("import-lock").disabled) return;
    if (input.value === "" || input.getAttribute("data-auto") === "1") {
      input.value = state.selected;
      input.setAttribute("data-auto", "1");
    }
  }

  // The client-side checks: only the obvious. The engine decides the rest.
  function collect() {
    var adapter = currentAdapter();
    if (!adapter) return { problem: "No source adapter is available: the server's list has not loaded." };
    var missing = [];
    var picked = [];
    var bytes = 0;
    state.fields.files.forEach(function (f) {
      var file = f.input.files && f.input.files[0];
      if (file) {
        picked.push({ name: f.name, file: file });
        bytes += file.size;
      } else if (f.required) {
        missing.push(f.name);
      }
    });
    if (missing.length) return { problem: "Choose a file for: " + missing.join(", ") + "." };
    var options = Object.create(null);
    var empty = [];
    var padded = [];
    state.fields.options.forEach(function (f) {
      var value = f.input.value;
      if (value.trim() === "") {
        if (f.required) empty.push(f.name);
      } else if (value !== value.trim()) {
        padded.push(f.name);
      } else {
        options[f.name] = value;
      }
    });
    if (empty.length) return { problem: "Fill in: " + empty.join(", ") + "." };
    if (padded.length) return { problem: "Remove leading or trailing spaces from " + padded.join(", ") + "." };
    var correction = $("import-correction").checked;
    var supersedes = $("import-supersedes").value.trim();
    var reason = $("import-reason").value;
    if (correction && !supersedes) return { problem: "A correction needs the id of the revision it supersedes." };
    if (correction && reason.trim() === "") return { problem: "A correction needs a reason." };
    if (bytes > MAX_FILE_BYTES) {
      return { problem: "The chosen files are " + bytes + " bytes in all; the server accepts at most 2 MiB (" + MAX_FILE_BYTES + " bytes) in one import. Nothing was sent." };
    }
    return { profile: adapter.profile, picked: picked, options: options, correction: correction, supersedes: supersedes, reason: reason };
  }

  // A file's bytes as text, exactly: strict UTF-8 that keeps a leading byte
  // order mark (the server decides what a BOM means). A file that is not
  // UTF-8 is refused here rather than sent with replacement characters.
  function readText(file) {
    return new Promise(function (resolve, reject) {
      var reader = new FileReader();
      reader.addEventListener("load", function () {
        try {
          resolve(new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(reader.result));
        } catch (e) {
          reject({ notUtf8: file.name });
        }
      });
      reader.addEventListener("error", function () { reject(reader.error); });
      reader.addEventListener("abort", function () { reject(reader.error); });
      reader.readAsArrayBuffer(file);
    });
  }

  function lockImport(locked) {
    $("import-lock").disabled = locked;
    state.busy = locked && !state.confirm;
  }

  function importRefusal(message) {
    clear($("import-result")).appendChild(errorBlock([message]));
    setStatus(message, true);
  }

  function onImportSubmit(event) {
    event.preventDefault();
    if (state.busy || state.confirm) return;
    clear($("import-result"));
    var c = collect();
    if (c.problem) {
      importRefusal(c.problem);
      return;
    }
    if (c.correction) showConfirm(c);
    else sendImport(c);
  }

  // A correction is sent only after the person has seen what it supersedes.
  function showConfirm(c) {
    state.confirm = c;
    lockImport(true);
    var known = state.known[c.supersedes];
    var superseded = c.supersedes + (known
      ? " (" + text(known.property) + ", " + text(known.period) + ", revision " + text(known.revision) + ")"
      : " (not in the loaded list)");
    var pairs = [["Profile", c.profile]];
    Object.keys(c.options).forEach(function (name) { pairs.push([name, c.options[name]]); });
    c.picked.forEach(function (p) { pairs.push(["File " + p.name, p.file.name + ", " + p.file.size + " bytes"]); });
    pairs.push(["Supersedes", superseded, "mono"], ["Reason", c.reason]);
    clear($("import-confirm-body")).appendChild(dl(pairs));
    $("import-confirm").hidden = false;
    $("import-confirm-h").focus();
    setStatus("Check the correction below, then confirm or cancel. Nothing has been sent.");
  }

  function endConfirm() {
    state.confirm = null;
    $("import-confirm").hidden = true;
    clear($("import-confirm-body"));
  }

  function cancelConfirm() {
    endConfirm();
    lockImport(false);
    setStatus("Correction cancelled. Nothing was sent.");
    $("import-submit").focus();
  }

  function sendImport(c) {
    var seq = ++state.importSeq;
    var wasConfirm = !!state.confirm;
    endConfirm();
    lockImport(true);
    clear($("import-result"));
    setStatus("Reading the chosen files…");
    var reading = Promise.all(c.picked.map(function (p) { return readText(p.file); })).then(function (texts) {
      var files = Object.create(null);
      c.picked.forEach(function (p, i) { files[p.name] = texts[i]; });
      var payload = { profile: c.profile, files: files, options: c.options };
      if (c.correction) {
        payload.supersedes = c.supersedes;
        payload.reason = c.reason;
      }
      // The server's cap is on the body: the files' text once JSON-escaped.
      var size = new TextEncoder().encode(JSON.stringify(payload)).length;
      if (size > MAX_FILE_BYTES) {
        return { failure: "With JSON escaping the request would be " + size + " bytes; the server accepts at most 2 MiB (" + MAX_FILE_BYTES + " bytes). Nothing was sent." };
      }
      if (seq === state.importSeq) setStatus("Sending " + c.picked.length + (c.picked.length === 1 ? " file" : " files") + " to the server…");
      return calc("import_period", payload, IMPORT_TIMEOUT_MS);
    }, function (e) {
      return { failure: e && e.notUtf8 ? e.notUtf8 + " is not UTF-8 text. Nothing was sent." : "A chosen file could not be read. Nothing was sent." };
    });
    return reading.then(function (res) {
      if (seq !== state.importSeq) return null;
      return finishImport(seq, res, wasConfirm);
    }).catch(function () {
      if (seq !== state.importSeq) return;
      importRefusal("The import ended unexpectedly. Reload the list to see whether a revision was stored.");
      lockImport(false);
    });
  }

  function finishImport(seq, res, wasConfirm) {
    var out = clear($("import-result"));
    if (!isEnvelope(res)) {
      var lines = describeFailure(res);
      if (res.timedOut) lines.push("The server may already have stored the period. Reload the list before trying again.");
      setStatus("Import failed: " + lines.join(" · "), true);
      out.appendChild(errorBlock(lines));
      lockImport(false);
      $("import-submit").focus();
      return null;
    }
    var envelope = res.body;
    if (envelope.status === "refused") {
      var error = envelope.error || {};
      setStatus("import_period was refused: " + text(error.code) + ": " + text(error.message), true);
      out.appendChild(refusalBlock(envelope));
      out.appendChild(citationRow(citationOf(envelope), envelope.trace_id, null, true));
      lockImport(false);
      $("import-submit").focus();
      return null;
    }
    var result = envelope.result || {};
    var id = typeof result.revision_id === "string" ? result.revision_id : "";
    // The form keeps its values; the file inputs are emptied.
    state.fields.files.forEach(function (f) { f.input.value = ""; });
    out.appendChild(el("h3", { text: wasConfirm ? "Correction loaded" : "Period loaded" }));
    out.appendChild(statusLine(envelope.status));
    out.appendChild(dl([["Revision id", text(result.revision_id), "mono"], ["Trace id", text(envelope.trace_id), "mono"]]));
    out.appendChild(citationRow(citationOf(envelope), envelope.trace_id, null, true));
    var sources = el("div");
    out.appendChild(sources);
    setStatus("Loaded revision " + id + ": " + text(envelope.status) + ". Reading its recorded sources…");
    var recorded = typeof envelope.trace_id === "string"
      ? calc("get_provenance", { trace_id: envelope.trace_id })
      : Promise.resolve({ failure: "The envelope carries no trace id." });
    return recorded.then(function (prov) {
      if (seq !== state.importSeq) return null;
      sources.appendChild(el("h3", { text: "Recorded sources" }));
      var record = isEnvelope(prov) && prov.body.status !== "refused" && prov.body.result ? prov.body.result.revision : null;
      if (record) {
        sourcesBlock(record.sources, false).forEach(function (node) { sources.appendChild(node); });
      } else {
        sources.appendChild(errorBlock(["The recorded sources could not be read back: " + (isEnvelope(prov) ? text((prov.body.error || {}).code) : describeFailure(prov).join(" · "))]));
      }
      return loadRevisions();
    }).then(function () {
      if (seq !== state.importSeq) return;
      lockImport(false);
      if (id) selectRevision(id, wasConfirm ? "Correction loaded. " : "Period loaded. ");
    });
  }

  // ---------------------------------------------------------------- trace

  function lookupTrace(id) {
    id = id.trim();
    if (!id) {
      setStatus("Paste a trace id first.", true);
      return;
    }
    $("trace-input").value = id;
    var seq = ++state.traceSeq;
    setStatus("Looking up trace " + id + "…");
    calc("get_provenance", { trace_id: id }).then(function (res) {
      if (seq !== state.traceSeq) return;
      var body = clear($("trace-body"));
      if (!isEnvelope(res)) {
        var lines = describeFailure(res);
        setStatus("Trace lookup failed: " + lines.join(" · "), true);
        body.appendChild(errorBlock(lines));
      } else if (res.body.status === "refused") {
        var error = res.body.error || {};
        setStatus("Trace lookup was refused: " + text(error.code) + ": " + text(error.message), true);
        body.appendChild(refusalBlock(res.body));
        appendProvenance(body, res.body, { id: "trace-citation", heading: "Citation for this lookup", lookup: false });
      } else {
        renderTrace(body, res.body);
        setStatus("Trace " + id + ": " + text(((res.body.result || {}).trace || {}).function) + ", " + text(((res.body.result || {}).trace || {}).status) + ".");
      }
      $("trace-h").focus();
    });
  }

  function renderTrace(body, envelope) {
    var result = envelope.result || {};
    var trace = result.trace || {};
    body.appendChild(statusLine(trace.status));
    var pairs = [
      ["Function", text(trace.function) + "@" + text(trace.version)],
      ["Producer version", text(trace.producer_version)],
      ["Caller", text(trace.caller)],
      ["Error code", orNone(trace.error_code)],
      ["Trace id", text(trace.trace_id), "mono"],
      ["Input sha256", text(trace.input_sha256), "mono"],
      ["Output sha256", orNone(trace.output_sha256), "mono"],
      ["Started", text(trace.started_at)],
      ["Finished", text(trace.finished_at)]
    ];
    if (trace.revision_id) pairs.push(["Revision id", text(trace.revision_id), "mono"]);
    if (trace.report_id) pairs.push(["Report id", text(trace.report_id), "mono"]);
    body.appendChild(dl(pairs));
    var revision = result.revision;
    if (revision && typeof revision === "object") {
      body.appendChild(el("h3", { text: "Revision" }));
      body.appendChild(dl([
        ["Revision id", text(revision.id), "mono"],
        ["Property", text(revision.property)],
        ["Period", text(revision.period)],
        ["Revision", text(revision.revision)],
        ["Supersedes", orNone(revision.parent_id), "mono"],
        ["Reason", orNone(revision.reason)],
        ["Input sha256", text(revision.input_sha256), "mono"]
      ]));
      body.appendChild(el("h3", { text: "Recorded sources" }));
      sourcesBlock(revision.sources, true).forEach(function (node) { body.appendChild(node); });
    }
    var report = result.report;
    if (report && typeof report === "object") {
      body.appendChild(el("h3", { text: "Report" }));
      body.appendChild(dl([
        ["Report id", text(report.id), "mono"],
        ["Created", text(report.created_at)],
        ["Body sha256", text(report.body_sha256), "mono"]
      ]));
    }
    appendProvenance(body, envelope, { id: "trace-citation", heading: "Citation for this lookup", lookup: false });
  }

  // ---------------------------------------------------------------- start

  function start() {
    request("GET", ".well-known/oauth-protected-resource").then(function (res) {
      var resource = !res.failure && res.status === 200 && res.body && typeof res.body.resource === "string"
        ? res.body.resource
        : window.location.origin;
      $("server-url").textContent = resource;
    });
    setStatus("Connecting…");
    return calc("describe", {}).then(function (res) {
      if (!isEnvelope(res)) {
        var lines = describeFailure(res);
        setStatus(lines.join(" · "), true);
        if (res.status === 401) $("key-input").focus();
        return;
      }
      if (getKey()) $("key-panel").hidden = false;
      setAdapters(res.body.result);
      return loadRevisions();
    });
  }

  function wire() {
    $("key-form").addEventListener("submit", function (event) {
      event.preventDefault();
      var input = $("key-input");
      var value = input.value.trim();
      input.value = "";
      if (!value) {
        setStatus("Paste a key first.", true);
        return;
      }
      if (!KEY_CHARS.test(value)) {
        setStatus("The key contains characters an API key cannot have.", true);
        return;
      }
      setKey(value);
      start();
    });
    $("key-forget").addEventListener("click", function () {
      setKey("");
      setStatus("Key forgotten for this tab.");
    });
    $("import-profile").addEventListener("change", renderFields);
    $("import-form").addEventListener("submit", onImportSubmit);
    $("import-correction").addEventListener("change", function () {
      var on = $("import-correction").checked;
      $("import-correction-fields").hidden = !on;
      if (on) prefillSupersedes();
    });
    $("import-supersedes").addEventListener("input", function () {
      $("import-supersedes").removeAttribute("data-auto");
    });
    $("import-confirm-yes").addEventListener("click", function () {
      if (state.confirm) sendImport(state.confirm);
    });
    $("import-confirm-no").addEventListener("click", cancelConfirm);
    $("trace-form").addEventListener("submit", function (event) {
      event.preventDefault();
      lookupTrace($("trace-input").value);
    });
    $("reload").addEventListener("click", function () { loadRevisions(); });
    $("revision-form").addEventListener("submit", function (event) {
      event.preventDefault();
      selectRevision($("revision-input").value);
    });
    start();
  }

  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", wire);
  else wire();
})();
