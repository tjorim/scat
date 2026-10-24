// A deliberately small, fictional catalog. This is not the Rust search engine.
(() => {
  const scripts = [
    {
      name: "backup.sh",
      path: "ops/backup.sh",
      language: "shell",
      owner: "platform",
      purpose: "Back up application data to shared storage.",
    },
    {
      name: "verify_backup.py",
      path: "ops/verify_backup.py",
      language: "python",
      owner: "platform",
      purpose: "Check the latest backup and report missing files.",
    },
    {
      name: "deploy.sh",
      path: "release/deploy.sh",
      language: "shell",
      owner: "platform",
      purpose: "Deploy a reviewed application build.",
    },
    {
      name: "storage.py",
      path: "lib/storage.py",
      language: "python",
      owner: "infra",
      purpose: "Shared storage helpers for backups and exports.",
    },
    {
      name: "report.py",
      path: "reports/report.py",
      language: "python",
      owner: "analytics",
      purpose: "Summarize the daily processing run.",
    },
  ];
  const query = document.querySelector("#query");
  const results = document.querySelector("#results");
  const count = document.querySelector("#count");
  const command = document.querySelector("#command");
  const status = document.querySelector("#copy-status");
  const filters = [...document.querySelectorAll("[data-language]")];
  let language = "all";
  const shellQuote = (value) => `'${value.replaceAll("'", "'\\''")}'`;

  function render() {
    const text = query.value.trim();
    const terms = text.toLowerCase().split(/\s+/).filter(Boolean);
    const matches = scripts.filter((script) => {
      const haystack = Object.values(script).join(" ").toLowerCase();
      return (
        (language === "all" || language === script.language) &&
        terms.every((term) => haystack.includes(term))
      );
    });
    results.replaceChildren();
    for (const script of matches) {
      const row = document.createElement("div");
      row.className = "sample-row";
      const name = document.createElement("div");
      name.className = "sample-name";
      name.textContent = script.name;
      const path = document.createElement("span");
      path.className = "sample-path";
      path.textContent = `${script.path} · ${script.owner}`;
      name.append(path);
      const purpose = document.createElement("div");
      purpose.className = "sample-purpose";
      purpose.textContent = script.purpose;
      const badge = document.createElement("span");
      badge.className = "sample-lang";
      badge.textContent = script.language;
      row.append(name, purpose, badge);
      results.append(row);
    }
    if (!matches.length) {
      const empty = document.createElement("p");
      empty.className = "demo-empty";
      empty.textContent =
        "No sample scripts match. Try “backup” or choose another language.";
      results.append(empty);
    }
    count.textContent = `${matches.length} ${matches.length === 1 ? "script" : "scripts"}`;
    command.textContent = `scat search${text ? ` ${shellQuote(text)}` : ""}${language === "all" ? "" : ` --lang ${language}`} --limit 50`;
    status.textContent = "";
    for (const button of filters)
      button.setAttribute(
        "aria-pressed",
        String(button.dataset.language === language),
      );
  }
  query.addEventListener("input", render);
  for (const button of filters)
    button.addEventListener("click", () => {
      language = button.dataset.language;
      render();
    });
  document.querySelector("#copy").addEventListener("click", async () => {
    try {
      await navigator.clipboard.writeText(command.textContent);
      status.textContent = "Copied.";
    } catch {
      status.textContent =
        "Clipboard unavailable. Select and copy the command above.";
    }
  });
  render();
})();
