// boxscore workbench: a read-only view over the server's /v1/calc routes.
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
  var KEY_STORAGE = "boxscore.workbench.apiKey";

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

  // selected: the revision under review. reviewSeq and listSeq number each
  // request a loader starts; only the latest one's response is rendered, so
  // overlapping requests (a double click, a second Reload) render once.
  var state = { selected: null, reviewSeq: 0, listSeq: 0 };

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

  // Resolves to { status, body, retryAfter } or { failure } and never throws.
  function request(method, path, payload) {
    var controller = new AbortController();
    var timer = window.setTimeout(function () { controller.abort(); }, TIMEOUT_MS);
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
      return {
        failure: error && error.name === "AbortError"
          ? "No answer from the server within " + TIMEOUT_MS / 1000 + " seconds."
          : "Network failure: the server could not be reached."
      };
    }).finally(function () {
      window.clearTimeout(timer);
    });
  }

  function calc(name, input) {
    // Relative, so a server behind a path prefix (a public URL ending in
    // /boxscore, say) is called under that prefix: the page is served at a
    // directory URL ending in a slash.
    return request("POST", "v1/calc/" + name, input);
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

  function selectRevision(id) {
    id = id.trim();
    if (!id) {
      setStatus("Paste a revision id first.", true);
      return;
    }
    state.selected = id;
    var seq = ++state.reviewSeq;
    markSelected(id);
    $("review").hidden = false;
    setStatus("Reviewing revision " + id + "…");
    calc("review_period", { revision_id: id }).then(function (res) {
      if (seq !== state.reviewSeq) return;
      var body = clear($("review-body"));
      if (!isEnvelope(res)) {
        var lines = describeFailure(res);
        setStatus("Review failed: " + lines.join(" · "), true);
        body.appendChild(errorBlock(lines));
      } else {
        renderReview(body, res.body);
        setStatus("Review of revision " + id + ": " + text(res.body.status) + ".", res.body.status === "refused");
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

  // The engine's citation line, then the whole envelope.
  function appendProvenance(body, envelope) {
    body.appendChild(el("h3", { text: "Citation" }));
    var line = typeof envelope.citation === "string" ? envelope.citation : citationFrom(envelope);
    var code = el("code", { id: "citation", text: line });
    var copy = el("button", { type: "button", text: "Copy" });
    copy.addEventListener("click", function () { copyText(code, line); });
    body.appendChild(el("div", { className: "citation" }, [code, copy]));
    var raw = el("pre", { className: "raw", text: JSON.stringify(envelope, null, 2) });
    body.appendChild(el("details", null, [el("summary", { text: "Raw envelope" }), raw]));
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
