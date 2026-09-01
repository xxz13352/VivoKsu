import { createCursorControls, createElement, renderPageState } from "../components.js";

const UUID_V7 = /^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
const V1_TRACE_REF = /^v1:([1-9][0-9]*)$/;
const V2_TRACE_REF = /^v2:([0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12})$/;
const USER_FILTERS = Object.freeze(["from", "to", "status", "q"]);
const RUN_FILTERS = Object.freeze(["userId", "kind", "status", "from", "to", "partition", "errorCode", "q"]);
const EXPORT_FILTERS = Object.freeze(["userId", "kind", "status", "from", "to", "partition", "errorCode", "q"]);
const AUDIT_FILTER_FIELDS = Object.freeze([
  Object.freeze({ name: "from", label: "开始时间", maxlength: 40 }),
  Object.freeze({ name: "to", label: "结束时间", maxlength: 40 }),
  Object.freeze({ name: "status", label: "状态", maxlength: 32 }),
  Object.freeze({ name: "kind", label: "操作类型", maxlength: 64 }),
  Object.freeze({ name: "userId", label: "用户 ID", maxlength: 128 }),
  Object.freeze({ name: "partition", label: "分区", maxlength: 64 }),
  Object.freeze({ name: "errorCode", label: "错误码", maxlength: 128 }),
  Object.freeze({ name: "q", label: "关键词", maxlength: 256 }),
]);
const PAGE_LIMIT = 50;
const OUTPUT_LIMIT = 50;
const RUN_OUTCOMES = new Set([
  "running",
  "success",
  "failed",
  "canceled",
  "denied",
  "aborted",
  "unknown",
]);
const EVENT_STATUSES = new Set(["started", "success", "failed", "canceled", "skipped", "unknown"]);
const DISPLAY_STATUSES = new Set([...RUN_OUTCOMES, ...EVENT_STATUSES]);

/**
 * Reusable audit workspace. The owner must call deactivate before another page owns the workspace.
 * Every activation owns one abort generation so late responses cannot replace a newer route.
 */
export function createAuditPage(context = {}) {
  const { document, window, api, navigate } = context;
  if (!document?.createElement || !window?.AbortController) {
    throw new TypeError("createAuditPage requires a browser document and window");
  }
  if (!api || typeof navigate !== "function") {
    throw new TypeError("createAuditPage requires api and navigate");
  }

  const element = createElement(document, "section", { className: "audit-page", "data-audit-page": "true" });
  let activation = 0;
  let controller = null;
  let externalSignal = null;
  let externalAbort = null;
  let listeners = [];
  let disposers = [];
  let destroyed = false;
  let currentRoute = null;
  let exportPending = false;

  function listen(target, type, handler, options) {
    target.addEventListener(type, handler, options);
    listeners.push(() => target.removeEventListener(type, handler, options));
  }

  function clearRenderBindings() {
    for (const dispose of listeners.splice(0)) dispose();
    for (const dispose of disposers.splice(0)) dispose();
  }

  function stopRequest() {
    controller?.abort();
    controller = null;
    if (externalSignal && externalAbort) externalSignal.removeEventListener("abort", externalAbort);
    externalSignal = null;
    externalAbort = null;
  }

  function beginActivation(signal) {
    stopRequest();
    clearRenderBindings();
    exportPending = false;
    const generation = ++activation;
    controller = new window.AbortController();
    externalSignal = signal ?? null;
    externalAbort = () => controller?.abort();
    if (externalSignal?.aborted) controller.abort();
    else externalSignal?.addEventListener("abort", externalAbort, { once: true });
    return { generation, signal: controller.signal };
  }

  function isCurrent(generation, signal) {
    return !destroyed && generation === activation && !signal.aborted;
  }

  function requireApi(name) {
    if (typeof api[name] !== "function") throw new Error(`审计 API ${name} 未接入。`);
  }

  function deactivate() {
    activation += 1;
    stopRequest();
    clearRenderBindings();
  }

  function destroy() {
    if (destroyed) return;
    destroyed = true;
    deactivate();
    element.remove();
  }

  async function activate(route, signal) {
    if (destroyed) throw new Error("audit page is destroyed");
    currentRoute = normalizeRoute(route);
    const request = beginActivation(signal);
    element.removeAttribute("data-page-state");
    const routeState = classifyRoute(currentRoute);
    if (routeState.kind === "invalid") {
      renderInvalidRoute(routeState.message);
      return;
    }

    const loading = renderPageState(element, {
      state: "loading",
      title: "正在加载操作审计",
      message: "正在读取服务端持久化证据。",
    });
    disposers.push(loading.destroy);
    try {
      if (routeState.kind === "users") await loadUsers(request);
      if (routeState.kind === "runs") await loadRuns(request);
      if (routeState.kind === "run") await loadRun(request);
      if (routeState.kind === "event") await loadEvent(request, routeState.output);
    } catch (error) {
      if (!isCurrent(request.generation, request.signal) || isAbort(error)) return;
      renderRequestError(error, signal);
    }
  }

  async function loadUsers(request) {
    requireApi("getTraceUsers");
    const response = await api.getTraceUsers(buildQuery(currentRoute, USER_FILTERS, true), {
      signal: request.signal,
    });
    if (!isCurrent(request.generation, request.signal)) return;
    const page = requirePage(response, "用户汇总");
    renderUsers(page);
  }

  async function loadRuns(request) {
    requireApi("getTraceRuns");
    const response = await api.getTraceRuns(buildQuery(currentRoute, RUN_FILTERS, true), {
      signal: request.signal,
    });
    if (!isCurrent(request.generation, request.signal)) return;
    const page = requirePage(response, "运行列表");
    renderRuns(page);
  }

  async function loadRun(request) {
    requireApi("getTraceRun");
    const detail = await api.getTraceRun(currentRoute.runId, { signal: request.signal });
    if (!isCurrent(request.generation, request.signal)) return;
    renderRunDetail(requireRunDetail(detail, currentRoute));
  }

  async function loadEvent(request, showOutput) {
    requireApi("getTraceEvent");
    const detail = await api.getTraceEvent(currentRoute.runId, currentRoute.eventId, {
      signal: request.signal,
    });
    if (!isCurrent(request.generation, request.signal)) return;
    const normalized = requireEventDetail(detail, currentRoute);
    const outputLoaders = renderEventDetail(normalized, showOutput, request);
    if (showOutput) await Promise.all(outputLoaders.map((load) => load()));
  }

  function renderUsers(page) {
    const content = createElement(document, "div", { className: "audit-content" });
    content.append(renderBreadcrumbs("users"), renderLevelHeading("用户汇总", "按用户进入持久化操作记录。"));
    content.append(createAuditFilterForm());
    content.append(createExportButton());
    if (page.items.length === 0) {
      content.append(createEmptyState(document, "当前筛选下没有用户操作记录。"));
    } else {
      const list = createElement(document, "ul", { className: "audit-list audit-user-list" });
      page.items.forEach((user) => {
        requireUserSummary(user);
        const focusId = `audit-user-${user.user_id}`;
        const label = displayUser(user);
        const button = createElement(document, "button", {
          type: "button",
          className: "audit-row-button",
          "data-audit-action": "open-user",
          "data-router-focus-id": focusId,
        }, [
          createElement(document, "strong", {}, label),
          createElement(document, "span", { className: "muted" }, `账号：${provided(user?.username)}`),
          createElement(document, "span", {}, `操作 ${finiteNumber(user?.operation_count)} · 失败 ${finiteNumber(user?.failed_count)}`),
          user?.last_operation
            ? createElement(document, "span", { className: "audit-last-operation" }, [
              `最近操作：${provided(user.last_operation.title)} · `,
              statusBadge(document, user.last_operation.outcome),
              ` · ${formatTime(user.last_operation.started_at_ms)}`,
            ])
            : createElement(document, "span", { className: "muted" }, "最近操作：未提供"),
          createElement(document, "span", { className: "muted" }, `最后活动：${formatTime(user?.last_activity_at_ms)}`),
        ]);
        listen(button, "click", () => navigateTo({
          ...currentRoute,
          view: "audit",
          level: "user",
          userId: String(user.user_id),
          runId: null,
          eventId: null,
          stream: null,
          cursor: null,
        }, focusId));
        list.append(createElement(document, "li", {}, button));
      });
      content.append(list);
    }
    appendPagination(content, page.next_cursor);
    element.replaceChildren(content);
  }

  function renderRuns(page) {
    const content = createElement(document, "div", { className: "audit-content" });
    content.append(renderBreadcrumbs("runs"), renderLevelHeading("操作记录", "此层不读取命令输出。"));
    content.append(createAuditFilterForm());
    content.append(createExportButton());
    if (page.items.length === 0) {
      content.append(createEmptyState(document, "当前筛选下没有操作记录。"));
    } else {
      const list = createElement(document, "ul", { className: "audit-list audit-run-list" });
      page.items.forEach((run) => {
        requireRunSummary(run);
        const focusId = `audit-run-${stableFocusKey(run.trace_ref)}`;
        const button = createElement(document, "button", {
          type: "button",
          className: "audit-row-button",
          "data-audit-action": "open-run",
          "data-router-focus-id": focusId,
        }, [
          createElement(document, "strong", {}, provided(run.title)),
          statusBadge(document, run.outcome),
          createElement(document, "span", {}, `工具：${provided(run.operation_kind)}`),
          createElement(document, "code", {}, run.trace_ref),
          createElement(document, "span", { className: "muted" }, `开始：${formatTime(run.started_at_ms)} · 耗时：${formatDuration(run.duration_ms)}`),
          createElement(document, "span", { className: "muted" }, `客户端：${provided(run.client_version)}`),
        ]);
        listen(button, "click", () => navigateTo({
          ...currentRoute,
          view: "audit",
          level: "run",
          runId: run.trace_ref,
          eventId: null,
          stream: null,
          cursor: null,
        }, focusId));
        list.append(createElement(document, "li", {}, button));
      });
      content.append(list);
    }
    appendPagination(content, page.next_cursor);
    element.replaceChildren(content);
  }

  function renderRunDetail(detail) {
    const { run } = detail;
    const content = createElement(document, "div", { className: "audit-content" });
    content.append(renderBreadcrumbs("run"), renderLevelHeading(provided(run.title), "按服务端 sequence 展示已持久化步骤。"));
    content.append(createAuditFilterForm(), createRunSummary(run), createExportButton());

    if (run.source_schema === 1 || detail.source_schema === 1 || detail.detail_available === false) {
      content.append(createElement(document, "section", {
        className: "page-state page-state-partial",
        role: "alert",
        "data-legacy-stop": "true",
      }, [
        createElement(document, "h3", {}, "旧版记录止于操作摘要"),
        createElement(document, "p", {}, "旧客户端未上传步骤数据"),
        createElement(document, "code", {}, provided(detail.detail_unavailable_reason)),
      ]));
      element.replaceChildren(content);
      return;
    }

    if (!run.trace_complete) {
      content.append(createElement(document, "section", { className: "page-state page-state-partial", role: "alert" }, [
        createElement(document, "h3", {}, "追踪不完整"),
        createElement(document, "p", {}, provided(run.trace_loss_reason)),
      ]));
    }

    if (detail.events.length === 0) {
      content.append(createEmptyState(document, "服务端尚未持久化步骤数据。"));
    } else {
      const list = createElement(document, "ol", { className: "audit-event-list" });
      detail.events.forEach((event) => {
        requireEventSummary(event, run);
        const focusId = `audit-event-${stableFocusKey(event.event_id)}`;
        const button = createElement(document, "button", {
          type: "button",
          className: "audit-row-button",
          "data-audit-action": "open-event",
          "data-router-focus-id": focusId,
        }, [
          createElement(document, "code", { "data-event-sequence": String(event.sequence) }, String(event.sequence)),
          createElement(document, "strong", {}, provided(event.step_name)),
          statusBadge(document, event.status),
          createElement(document, "span", {}, `类别：${provided(event.kind)} · 分区：${provided(event.partition_name)}`),
          createElement(document, "span", { className: "muted" }, `耗时：${formatDuration(event.duration_ms)}`),
          createElement(document, "span", {}, "查看详情"),
        ]);
        listen(button, "click", () => navigateTo({
          ...currentRoute,
          view: "audit",
          level: "command",
          eventId: event.event_id,
          stream: null,
          cursor: null,
        }, focusId));
        list.append(createElement(document, "li", {}, button));
      });
      content.append(list);
    }
    element.replaceChildren(content);
  }

  function createRunSummary(run) {
    return createElement(document, "section", { className: "audit-run-summary", "aria-label": "运行摘要" }, [
      statusBadge(document, run.outcome),
      createElement(document, "p", {}, `Trace：${run.trace_ref}`),
      createElement(document, "p", {}, `用户：${provided(run.user_name ?? run.username)}`),
      createElement(document, "p", {}, `工具：${provided(run.operation_kind)}`),
      createElement(document, "p", {}, `开始：${formatTime(run.started_at_ms)} · 结束：${formatTime(run.ended_at_ms)}`),
      createElement(document, "p", {}, `客户端：${provided(run.client_version)}`),
    ]);
  }

  function renderEventDetail(detail, showOutput, request) {
    const { run, event } = detail;
    const content = createElement(document, "div", { className: "audit-content audit-event-detail" });
    content.append(
      renderBreadcrumbs(showOutput ? "output" : "event"),
      renderLevelHeading(provided(event.step_name), showOutput
        ? "stdout 与 stderr 使用独立服务端游标读取。"
        : "仅展示服务端返回的步骤与命令证据。"),
      createAuditFilterForm(),
      createRunSummary(run),
      createExportButton(),
    );

    const evidence = createElement(document, "section", { className: "audit-evidence", "aria-label": "步骤执行证据" }, [
      createElement(document, "h3", {}, "步骤执行详情"),
      statusBadge(document, event.status),
      evidenceField(document, "result", "结果类别", event.status?.toUpperCase()),
      evidenceField(document, "kind", "阶段", event.kind),
      evidenceField(document, "partition", "分区", event.partition_name),
      evidenceField(document, "exit-code", "退出码", event.exit_code),
      evidenceField(document, "sequence", "步骤序号", event.sequence),
      evidenceField(document, "started-at", "开始时间", formatTime(event.started_at_ms)),
      evidenceField(document, "ended-at", "结束时间", formatTime(event.ended_at_ms)),
      evidenceField(document, "duration", "耗时", formatDuration(event.duration_ms)),
      evidenceField(document, "verification", "服务端验证", event.verification),
      evidenceField(document, "device-state", "设备停止状态", event.device_state),
      evidenceField(document, "retry-safe", "可安全重试", booleanEvidence(event.retry_safe)),
      evidenceField(document, "error-class", "错误类别", event.error_class),
      evidenceField(document, "error-code", "错误码", event.error_code),
      evidenceField(document, "error-message", "失败原因", event.error_message),
      evidenceList(document, "remedies", "处理建议", event.remedies),
      evidenceList(
        document,
        "credential-redactions",
        "凭据移除计数",
        event.credential_redactions?.map((item) => `${provided(item?.kind)}: ${finiteNumber(item?.count)}`),
      ),
      evidenceField(document, "stdout-count", "stdout 持久化块声明", event.stdout_chunks),
      evidenceField(document, "stderr-count", "stderr 持久化块声明", event.stderr_chunks),
    ]);
    content.append(evidence);

    if (event.command) content.append(renderCommandEvidence(event.command));
    else content.append(createElement(document, "section", { className: "audit-command", "aria-label": "命令证据" }, [
      createElement(document, "h3", {}, "命令证据"),
      createElement(document, "p", {}, "未提供"),
    ]));

    const loaders = [];
    if (showOutput) {
      const outputs = createElement(document, "section", { className: "audit-output", "aria-label": "完整命令日志" }, [
        createElement(document, "h3", {}, "完整命令日志"),
      ]);
      for (const stream of ["stdout", "stderr"]) {
        const streamState = createOutputStream(stream, detail, request);
        loaders.push(streamState.load);
        outputs.append(streamState.element);
      }
      content.append(outputs);
    } else {
      const actions = createElement(document, "div", { className: "audit-output-actions" });
      for (const stream of ["stdout", "stderr"]) {
        const focusId = `audit-open-${stream}`;
        const button = createElement(document, "button", {
          type: "button",
          className: "button",
          "data-audit-action": "open-output",
          "data-stream": stream,
          "data-router-focus-id": focusId,
        }, `查看 ${stream}`);
        listen(button, "click", () => navigateTo({
          ...currentRoute,
          level: "command",
          stream,
          cursor: null,
        }, focusId));
        actions.append(button);
      }
      content.append(actions);
    }

    element.replaceChildren(content);
    return loaders;
  }

  function renderCommandEvidence(command) {
    return createElement(document, "section", { className: "audit-command", "aria-label": "命令证据" }, [
      createElement(document, "h3", {}, "命令证据"),
      codeEvidence(document, "program", "程序", command.program),
      codeEvidence(document, "display-command", "完整命令", command.display_command),
      codeListEvidence(document, "argv", "argv", command.argv),
      codeEvidence(document, "working-directory", "工作目录", command.working_directory),
      codeListEvidence(document, "paths", "路径", command.paths),
      codeListEvidence(document, "urls", "URL", command.urls),
      codeEvidence(document, "serial", "设备序列号", command.serial),
    ]);
  }

  function createOutputStream(stream, detail, request) {
    const title = createElement(document, "h3", {}, stream);
    const output = createElement(document, "pre", {
      className: "audit-output-code",
      tabindex: "0",
      "aria-label": `${stream} 输出`,
      "data-output-stream": stream,
    });
    const stateText = createElement(document, "p", {
      className: "muted",
      "data-output-state": stream,
    }, "尚未读取");
    const errorText = createElement(document, "p", {
      className: "page-state page-state-error",
      role: "alert",
      hidden: true,
      "data-output-error": stream,
    });
    const loadButton = createElement(document, "button", {
      type: "button",
      className: "button",
      "data-load-output": stream,
    }, `加载 ${stream}`);
    const panel = createElement(document, "section", {
      className: "audit-output-stream",
      "data-active-stream": currentRoute.stream === stream ? "true" : "false",
    }, [title, output, stateText, errorText, loadButton]);
    const state = {
      afterChunk: -1,
      chunks: [],
      complete: false,
      next: true,
      loading: false,
      failed: false,
    };

    async function load() {
      if (state.loading || state.complete || request.signal.aborted) return;
      state.loading = true;
      state.failed = false;
      loadButton.disabled = true;
      loadButton.hidden = false;
      errorText.hidden = true;
      errorText.replaceChildren();
      stateText.textContent = "正在读取";
      try {
        requireApi("getTraceOutput");
        const page = await api.getTraceOutput(
          currentRoute.runId,
          currentRoute.eventId,
          { stream, afterChunk: state.afterChunk, limit: OUTPUT_LIMIT },
          { signal: request.signal },
        );
        if (!isCurrent(request.generation, request.signal)) return;
        const next = validateOutputPage(page, {
          stream,
          runId: detail.event.run_id,
          eventId: detail.event.event_id,
          afterChunk: state.afterChunk,
          expectedTotal: stream === "stdout" ? detail.event.stdout_chunks : detail.event.stderr_chunks,
        });
        state.chunks.push(...next.chunks);
        if (next.chunks.length > 0) state.afterChunk = next.chunks.at(-1).chunk_index;
        state.complete = next.output_complete;
        state.next = next.next_after_chunk !== null;
        output.textContent = state.chunks.length > 0
          ? state.chunks.map((chunk) => chunk.text).join("")
          : state.complete ? "(empty)" : "";
        stateText.textContent = state.complete
          ? "输出完整"
          : state.next ? `已读取至 chunk ${state.afterChunk}` : "输出尚未完整";
        loadButton.textContent = state.next ? `继续加载 ${stream}` : `重新检查 ${stream}`;
        loadButton.hidden = state.complete;
      } catch (error) {
        if (!isCurrent(request.generation, request.signal) || isAbort(error)) return;
        state.failed = true;
        errorText.hidden = false;
        errorText.textContent = safeErrorMessage(error);
        stateText.textContent = "输出读取失败";
        loadButton.hidden = false;
        loadButton.textContent = `重试 ${stream}`;
      } finally {
        state.loading = false;
        if (loadButton.isConnected && !state.complete) loadButton.disabled = false;
      }
    }

    listen(loadButton, "click", () => void load());
    return { element: panel, load };
  }

  function renderBreadcrumbs(current) {
    const order = [
      { id: "users", label: "用户", route: { view: "audit" } },
      { id: "runs", label: "操作", route: { ...currentRoute, level: "user", runId: null, eventId: null, stream: null, cursor: null } },
      { id: "run", label: "步骤", route: { ...currentRoute, level: "run", eventId: null, stream: null, cursor: null } },
      { id: "event", label: "执行详情", route: { ...currentRoute, level: "command", stream: null, cursor: null } },
      { id: "output", label: "命令日志", route: { ...currentRoute, level: "command", stream: currentRoute.stream ?? "stdout", cursor: null } },
    ];
    const currentIndex = order.findIndex((item) => item.id === current);
    const list = createElement(document, "ol", { className: "audit-breadcrumb-list" });
    order.slice(0, currentIndex + 1).forEach((item, index) => {
      const child = index === currentIndex
        ? createElement(document, "span", { "aria-current": "page" }, item.label)
        : breadcrumbButton(item);
      list.append(createElement(document, "li", {}, child));
    });
    return createElement(document, "nav", { className: "audit-breadcrumbs", "aria-label": "审计层级" }, list);
  }

  function breadcrumbButton(item) {
    const focusId = `audit-breadcrumb-${item.id}`;
    const button = createElement(document, "button", {
      type: "button",
      className: "audit-breadcrumb-button",
      "data-router-focus-id": focusId,
    }, item.label);
    listen(button, "click", () => navigateTo(item.route, focusId));
    return button;
  }

  function renderLevelHeading(title, description) {
    return createElement(document, "header", { className: "audit-level-heading" }, [
      createElement(document, "h2", { "data-route-heading": "true", tabindex: "-1" }, title),
      createElement(document, "p", { className: "muted" }, description),
    ]);
  }

  function createAuditFilterForm() {
    const form = createElement(document, "form", {
      className: "audit-filter-form",
      "aria-label": "审计筛选",
      "data-audit-filter-form": "true",
    });
    const fields = createElement(document, "div", { className: "audit-filter-fields" });
    for (const field of AUDIT_FILTER_FIELDS) {
      const id = `audit-filter-${field.name}`;
      fields.append(createElement(document, "label", { className: "audit-filter-field", for: id }, [
        field.label,
        createElement(document, "input", {
          id,
          name: field.name,
          type: field.name === "q" ? "search" : "text",
          autocomplete: "off",
          maxlength: String(field.maxlength),
          value: textOrNull(currentRoute?.[field.name]) ?? "",
          "data-audit-filter": field.name,
        }),
      ]));
    }
    const actions = createElement(document, "div", { className: "audit-filter-actions" });
    const submit = createElement(document, "button", {
      type: "submit",
      className: "button",
      "data-audit-filter-action": "submit",
      "data-router-focus-id": "audit-filter-submit",
    }, "应用筛选");
    const reset = createElement(document, "button", {
      type: "button",
      className: "button button-secondary",
      "data-audit-filter-action": "reset",
      "data-router-focus-id": "audit-filter-reset",
    }, "重置筛选");
    actions.append(submit, reset);
    form.append(fields, actions);
    listen(form, "submit", (event) => {
      event.preventDefault();
      const filters = readAuditFilters(form);
      navigateTo(auditFilterRoute(filters), "audit-filter-submit");
    });
    listen(reset, "click", () => {
      navigateTo(auditFilterRoute(Object.fromEntries(AUDIT_FILTER_FIELDS.map(({ name }) => [name, null]))), "audit-filter-reset");
    });
    return form;
  }

  function readAuditFilters(form) {
    return Object.fromEntries(AUDIT_FILTER_FIELDS.map(({ name }) => {
      const value = form.elements.namedItem(name)?.value?.trim() ?? "";
      return [name, value.length > 0 ? value : null];
    }));
  }

  function auditFilterRoute(filters) {
    const userId = filters.userId;
    const hasRunOnlyFilter = Boolean(filters.kind || filters.partition || filters.errorCode);
    return {
      view: "audit",
      level: userId || hasRunOnlyFilter ? "user" : "overview",
      userId,
      runId: null,
      eventId: null,
      stream: null,
      from: filters.from,
      to: filters.to,
      status: filters.status,
      kind: filters.kind,
      partition: filters.partition,
      errorCode: filters.errorCode,
      q: filters.q,
      cursor: null,
    };
  }

  function createExportButton() {
    const button = createElement(document, "button", {
      type: "button",
      className: "button audit-export",
      "data-audit-action": "export",
    }, "导出当前筛选 NDJSON");
    listen(button, "click", () => exportCurrent(button));
    return button;
  }

  function exportCurrent(button) {
    if (button.disabled || exportPending) return;
    exportPending = true;
    button.disabled = true;
    let anchor = null;
    let started = false;
    try {
      requireApi("getTraceExportUrl");
      const url = validateExportUrl(api.getTraceExportUrl(buildQuery(currentRoute, EXPORT_FILTERS, false)), window);
      anchor = createElement(document, "a", {
        href: url,
        download: "nwflash-traces.ndjson",
        hidden: true,
        rel: "noopener",
      });
      document.body.append(anchor);
      anchor.click();
      started = true;
      button.textContent = "导出已开始";
      context.announce?.("审计导出已准备。", { kind: "success" });
    } catch (error) {
      context.alert?.(safeErrorMessage(error), { title: "导出失败" });
    } finally {
      anchor?.remove();
      if (!started) {
        exportPending = false;
        if (button.isConnected) button.disabled = false;
      }
    }
  }

  function appendPagination(content, nextCursor) {
    const controls = createCursorControls({
      document,
      label: "审计分页",
      onPrevious: () => {},
      onNext: () => {
        if (typeof nextCursor === "string" && nextCursor.length > 0) {
          navigateTo({ ...currentRoute, cursor: nextCursor }, "audit-pagination-next");
        }
      },
    });
    const next = controls.element.querySelectorAll("button")[1];
    if (next) next.dataset.routerFocusId = "audit-pagination-next";
    controls.update({
      hasPrevious: false,
      hasNext: typeof nextCursor === "string" && nextCursor.length > 0,
      pageLabel: currentRoute.cursor ? "后续页" : "第 1 页",
    });
    disposers.push(controls.destroy);
    content.append(controls.element);
  }

  function navigateTo(route, focusId) {
    void Promise.resolve(navigate(normalizeRoute(route), { focusId })).catch((error) => {
      context.alert?.(safeErrorMessage(error), { title: "无法打开审计层级" });
    });
  }

  function renderInvalidRoute(message) {
    element.replaceChildren(createElement(document, "section", {
      className: "page-state page-state-error",
      role: "alert",
      "data-state": "error",
    }, [
      createElement(document, "h2", { "data-route-heading": "true", tabindex: "-1" }, "审计链接参数不完整"),
      createElement(document, "p", {}, message),
    ]));
  }

  function renderRequestError(error, signal) {
    const state = renderPageState(element, {
      state: "retry",
      title: "无法加载操作审计",
      message: safeErrorMessage(error),
      onRetry: () => void activate(currentRoute, signal),
    });
    disposers.push(state.destroy);
  }

  return Object.freeze({ element, activate, deactivate, destroy });
}

function normalizeRoute(route) {
  return { ...(route && typeof route === "object" ? route : {}), view: "audit" };
}

function classifyRoute(route) {
  const runId = textOrNull(route.runId);
  const eventId = textOrNull(route.eventId);
  const stream = textOrNull(route.stream);
  const level = textOrNull(route.level);
  const userId = textOrNull(route.userId);
  if (userId !== null && (!/^[1-9][0-9]*$/.test(userId) || !Number.isSafeInteger(Number(userId)))) {
    return invalid("用户标识无效，未发起查询。");
  }
  const cursor = textOrNull(route.cursor);
  if (eventId && (!runId || !V2_TRACE_REF.test(runId) || !UUID_V7.test(eventId))) {
    return invalid("步骤详情必须绑定有效的 V2 trace_ref 与 event_id。");
  }
  if (runId && !V1_TRACE_REF.test(runId) && !V2_TRACE_REF.test(runId)) {
    return invalid("运行标识必须是完整的 v1: 或 v2: trace_ref。");
  }
  if (stream && (!eventId || (stream !== "stdout" && stream !== "stderr"))) {
    return invalid("输出流必须绑定步骤且只能是 stdout 或 stderr。");
  }
  if (level === null || level === "overview") {
    return userId || runId || eventId || stream
      ? invalid("用户汇总层级不能携带下级标识。")
      : { kind: "users" };
  }
  if (level === "user") {
    return !runId && !eventId && !stream
      ? { kind: "runs" }
      : invalid("操作列表层级不能携带运行、步骤或输出标识。");
  }
  if (level === "run") {
    return runId && !eventId && !stream && !cursor
      ? { kind: "run" }
      : invalid("步骤列表层级必须只绑定 trace_ref，且不能携带列表游标。");
  }
  if (level === "event") {
    return runId && eventId && !stream && !cursor
      ? { kind: "event", output: false }
      : invalid("步骤详情层级必须绑定 V2 trace_ref 与 event_id，且不能携带输出状态。");
  }
  if (level === "command") {
    return runId && eventId && !cursor
      ? { kind: "event", output: stream !== null }
      : invalid("命令层级必须绑定 V2 trace_ref 与 event_id，且不能携带列表游标。");
  }
  return invalid("审计层级与标识不一致。");
}

function invalid(message) {
  return { kind: "invalid", message };
}

function buildQuery(route, keys, pagination) {
  const query = {};
  for (const key of keys) {
    const value = route[key];
    if (value !== null && value !== undefined && value !== "") query[key] = value;
  }
  if (pagination) {
    query.limit = PAGE_LIMIT;
    if (route.cursor) query.cursor = route.cursor;
  }
  return query;
}

function requirePage(value, label) {
  if (!value || !Array.isArray(value.items) || (value.next_cursor !== null && typeof value.next_cursor !== "string")) {
    throw new Error(`${label}响应格式无效。`);
  }
  return value;
}

function requireRunDetail(value, route) {
  if (!value || (value.source_schema !== 1 && value.source_schema !== 2) || !value.run || !Array.isArray(value.events)) {
    throw new Error("运行详情响应格式无效。");
  }
  requireRunSummary(value.run);
  if (value.source_schema !== value.run.source_schema || value.run.trace_ref !== route.runId) {
    throw new Error("运行详情与审计链接不一致。");
  }
  if (value.source_schema === 1) {
    if (value.detail_available !== false || value.run.run_id !== null || value.events.length !== 0) {
      throw new Error("旧版运行详情响应格式无效。");
    }
  } else if (value.detail_available !== true || value.run.run_id !== route.runId.slice(3)) {
    throw new Error("V2 运行详情响应格式无效。");
  }
  let previousSequence = 0;
  for (const event of value.events) {
    requireEventSummary(event, value.run);
    if (event.sequence <= previousSequence) throw new Error("步骤响应顺序无效。");
    previousSequence = event.sequence;
  }
  return value;
}

function requireUserSummary(user) {
  if (
    !user
    || !Number.isSafeInteger(user.user_id)
    || user.user_id < 1
    || typeof user.username !== "string"
    || typeof user.name !== "string"
  ) {
    throw new Error("用户汇总响应格式无效。");
  }
}

function requireRunSummary(run) {
  if (!run || (run.source_schema !== 1 && run.source_schema !== 2) || typeof run.trace_ref !== "string") {
    throw new Error("运行摘要响应格式无效。");
  }
  const validRef = run.source_schema === 1 ? V1_TRACE_REF.test(run.trace_ref) : V2_TRACE_REF.test(run.trace_ref);
  if (!validRef || !RUN_OUTCOMES.has(String(run.outcome))) throw new Error("运行摘要响应格式无效。");
}

function requireEventSummary(event, run) {
  if (!event || !UUID_V7.test(String(event.event_id)) || !Number.isSafeInteger(event.sequence) || event.sequence < 1) {
    throw new Error("步骤响应格式无效。");
  }
  if (event.run_id !== run.run_id || !EVENT_STATUSES.has(String(event.status))) {
    throw new Error("步骤与运行响应不一致。");
  }
}

function requireEventDetail(value, route) {
  if (!value?.run || !value?.event) throw new Error("步骤详情响应格式无效。");
  requireRunSummary(value.run);
  requireEventSummary(value.event, value.run);
  if (
    value.run.source_schema !== 2
    || value.run.trace_ref !== route.runId
    || value.run.run_id !== route.runId.slice(3)
    || (route.userId !== null && route.userId !== undefined && Number(route.userId) !== value.run.user_id)
    || value.event.event_id !== route.eventId
    || !isCommand(value.event.command)
    || !Array.isArray(value.event.remedies)
    || !Array.isArray(value.event.credential_redactions)
    || !Number.isSafeInteger(value.event.stdout_chunks)
    || value.event.stdout_chunks < 0
    || value.event.stdout_chunks > 200
    || !Number.isSafeInteger(value.event.stderr_chunks)
    || value.event.stderr_chunks < 0
    || value.event.stderr_chunks > 200
  ) {
    throw new Error("步骤详情与审计链接不一致。");
  }
  return value;
}

function isCommand(value) {
  if (value === null) return true;
  return Boolean(
    value
    && typeof value.program === "string"
    && typeof value.display_command === "string"
    && (value.working_directory === null || typeof value.working_directory === "string")
    && (value.serial === null || typeof value.serial === "string")
    && Array.isArray(value.argv)
    && value.argv.every((item) => typeof item === "string")
    && Array.isArray(value.paths)
    && value.paths.every((item) => typeof item === "string")
    && Array.isArray(value.urls)
    && value.urls.every((item) => typeof item === "string")
  );
}

function validateOutputPage(value, expected) {
  if (
    !value
    || value.run_id !== expected.runId
    || value.event_id !== expected.eventId
    || value.stream !== expected.stream
    || !Array.isArray(value.chunks)
    || (value.next_after_chunk !== null && !Number.isSafeInteger(value.next_after_chunk))
    || typeof value.output_complete !== "boolean"
  ) {
    throw new Error(`${expected.stream} 输出分页响应格式无效。`);
  }
  let expectedIndex = expected.afterChunk + 1;
  for (const chunk of value.chunks) {
    if (
      !chunk
      || chunk.event_id !== expected.eventId
      || chunk.stream !== expected.stream
      || chunk.chunk_index !== expectedIndex
      || typeof chunk.text !== "string"
    ) {
      throw new Error(`${expected.stream} 输出分页响应不连续。`);
    }
    expectedIndex += 1;
  }
  const lastIndex = value.chunks.length > 0 ? value.chunks.at(-1).chunk_index : expected.afterChunk;
  if (lastIndex >= expected.expectedTotal && expected.expectedTotal >= 0) {
    throw new Error(`${expected.stream} 输出超过事件声明总数。`);
  }
  if (value.next_after_chunk !== null && value.chunks.length === 0) {
    throw new Error(`${expected.stream} 输出游标未前进。`);
  }
  if (value.next_after_chunk !== null && value.next_after_chunk !== lastIndex) {
    throw new Error(`${expected.stream} 输出游标与持久化块不一致。`);
  }
  if (value.next_after_chunk !== null && lastIndex >= expected.expectedTotal - 1) {
    throw new Error(`${expected.stream} 输出游标超过事件声明总数。`);
  }
  if (value.output_complete && (value.next_after_chunk !== null || lastIndex !== expected.expectedTotal - 1)) {
    throw new Error(`${expected.stream} 输出未达到事件声明总数，不能标记完整。`);
  }
  return value;
}

function evidenceField(document, key, label, value) {
  return createElement(document, "p", { "data-evidence-field": key }, [
    createElement(document, "strong", {}, `${label}：`),
    provided(value),
  ]);
}

function evidenceList(document, key, label, values) {
  const list = Array.isArray(values) ? values : [];
  return createElement(document, "div", { "data-evidence-field": key }, [
    createElement(document, "strong", {}, `${label}：`),
    list.length > 0
      ? createElement(document, "ul", {}, list.map((value) => createElement(document, "li", {}, provided(value))))
      : createElement(document, "p", {}, "未提供"),
  ]);
}

function codeEvidence(document, key, label, value) {
  return createElement(document, "div", { "data-command-field": key }, [
    createElement(document, "strong", {}, `${label}：`),
    createElement(document, "pre", { className: "audit-code", tabindex: "0" }, provided(value)),
  ]);
}

function codeListEvidence(document, key, label, values) {
  const list = Array.isArray(values) ? values : [];
  return createElement(document, "div", { "data-command-field": key }, [
    createElement(document, "strong", {}, `${label}：`),
    createElement(document, "pre", { className: "audit-code", tabindex: "0" },
      list.length > 0 ? list.map((value) => provided(value)).join("\n") : "未提供"),
  ]);
}

function booleanEvidence(value) {
  if (value === true) return "是";
  if (value === false) return "否";
  return null;
}

function statusBadge(document, status) {
  const normalized = DISPLAY_STATUSES.has(String(status)) ? String(status) : "unknown";
  return createElement(document, "span", {
    className: `audit-status audit-status-${normalized}`,
    "data-status": normalized,
  }, normalized.toUpperCase());
}

function createEmptyState(document, message) {
  return createElement(document, "section", { className: "page-state page-state-empty", role: "status" }, [
    createElement(document, "h3", {}, "暂无数据"),
    createElement(document, "p", {}, message),
  ]);
}

function displayUser(user) {
  return provided(user?.name || user?.username);
}

function finiteNumber(value) {
  return Number.isSafeInteger(Number(value)) && Number(value) >= 0 ? String(Number(value)) : "未提供";
}

function formatTime(value) {
  if (!Number.isSafeInteger(Number(value)) || Number(value) < 0) return "未提供";
  try {
    return new Date(Number(value)).toISOString();
  } catch {
    return "未提供";
  }
}

function formatDuration(value) {
  return Number.isSafeInteger(Number(value)) && Number(value) >= 0 ? `${Number(value)} ms` : "未提供";
}

function provided(value) {
  return value === null || value === undefined || value === "" ? "未提供" : String(value);
}

function textOrNull(value) {
  return typeof value === "string" && value.length > 0 ? value : null;
}

function stableFocusKey(value) {
  const source = String(value);
  let first = 0x811c9dc5;
  let second = 0x9e3779b9;
  for (let index = 0; index < source.length; index += 1) {
    const code = source.charCodeAt(index);
    first = Math.imul(first ^ code, 0x01000193);
    second = Math.imul(second ^ code, 0x85ebca6b);
  }
  return `${(first >>> 0).toString(16).padStart(8, "0")}${(second >>> 0).toString(16).padStart(8, "0")}`;
}

function validateExportUrl(value, window) {
  if (typeof value !== "string" || !value.startsWith("/api/usage-logs/v2/export")) {
    throw new Error("导出地址响应格式无效。");
  }
  let parsed;
  try {
    parsed = new window.URL(value, window.location.origin);
  } catch {
    throw new Error("导出地址响应格式无效。");
  }
  if (
    parsed.origin !== window.location.origin
    || parsed.pathname !== "/api/usage-logs/v2/export"
    || parsed.hash
  ) {
    throw new Error("导出地址必须是同源审计端点。");
  }
  return `${parsed.pathname}${parsed.search}`;
}

function isAbort(error) {
  return error?.name === "AbortError" || error?.kind === "aborted" || error?.code === "ADMIN_ABORTED";
}

function safeErrorMessage(error) {
  return typeof error?.message === "string" && error.message.length > 0 ? error.message : "服务器未能返回数据。";
}
