// overview.js — Overview tab: stat cards, model table, mode switching

let statsSnapshot = null;
let currentOverviewMode = 'live';
let overviewRequestToken = 0;
let overviewRefreshTimer = null;

const CLEAR_MEMORY_BUTTON_LABEL = 'Clear in-memory stats';
const CLEAR_ALL_BUTTON_LABEL = 'Clear stats + data';
const EMPTY_MODELS_HTML = '<div style="color:var(--text-2);font-size:12px">No models yet</div>';

function updateOverview() {
  if (!statsSnapshot) return;

  const s = statsSnapshot.stats;
  const mode = statsSnapshot.mode || currentOverviewMode;
  const coverageLabel = document.getElementById('overview-coverage');
  const coverageStart = statsSnapshot.coverage_start ? new Date(statsSnapshot.coverage_start) : null;
  const coverageEnd = statsSnapshot.coverage_end ? new Date(statsSnapshot.coverage_end) : null;
  coverageLabel.textContent = formatCoverageLabel(mode, coverageStart, coverageEnd);

  const scoreEl = document.getElementById('health-score');
  scoreEl.textContent = Math.round(s.health_score);
  scoreEl.style.color = s.health_score >= 80 ? 'var(--green)' : s.health_score >= 50 ? 'var(--yellow)' : 'var(--red)';

  document.getElementById('health-label').textContent = s.health_label;
  const bar = document.getElementById('health-bar');
  bar.style.width = s.health_score + '%';
  bar.style.background = s.health_score >= 80 ? 'var(--green)' : s.health_score >= 50 ? 'var(--yellow)' : 'var(--red)';

  document.getElementById('total-requests').textContent = s.total_requests.toLocaleString();
  document.getElementById('requests-rate').textContent = s.requests_per_minute.toFixed(1) + ' req/min';

  const srEl = document.getElementById('success-rate');
  srEl.textContent = s.success_rate.toFixed(1) + '%';
  srEl.style.color = s.success_rate > 95 ? 'var(--green)' : s.success_rate > 80 ? 'var(--yellow)' : 'var(--red)';
  document.getElementById('error-count').textContent = s.total_errors + ' errors | ' + s.errors_last_5min + ' last 5min';

  const avgTtft = s.avg_ttft_ms / 1000;
  const avgEl = document.getElementById('avg-ttft');
  avgEl.textContent = avgTtft.toFixed(1) + 's';
  avgEl.style.color = avgTtft < 3 ? 'var(--green)' : avgTtft < 8 ? 'var(--yellow)' : 'var(--red)';
  document.getElementById('ttft-p95').textContent = 'P95: ' + (s.p95_ttft_ms / 1000).toFixed(1) + 's · P99: ' + (s.p99_ttft_ms / 1000).toFixed(1) + 's';

  document.getElementById('median-ttft').textContent = (s.median_ttft_ms / 1000).toFixed(1) + 's';
  document.getElementById('worst-ttft').textContent = 'Worst: ' + (s.worst_ttft_ms / 1000).toFixed(1) + 's';

  const stallEl = document.getElementById('total-stalls');
  stallEl.textContent = s.total_stalls;
  stallEl.style.color = s.total_stalls > 10 ? 'var(--red)' : s.total_stalls > 0 ? 'var(--yellow)' : 'var(--green)';
  document.getElementById('stall-time').textContent = s.total_stall_time_s.toFixed(1) + 's lost';

  const toEl = document.getElementById('total-timeouts');
  toEl.textContent = s.total_timeouts;
  toEl.style.color = s.total_timeouts > 0 ? 'var(--red)' : 'var(--green)';
  document.getElementById('conn-errors').textContent = s.total_connection_errors + ' conn errors';

  const totalTok = s.total_input_tokens + s.total_output_tokens;
  document.getElementById('total-tokens').textContent = totalTok > 1e6 ? (totalTok / 1e6).toFixed(1) + 'M' : totalTok > 1e3 ? (totalTok / 1e3).toFixed(1) + 'K' : totalTok;
  document.getElementById('token-breakdown').textContent = `in: ${fmt(s.total_input_tokens)} / out: ${fmt(s.total_output_tokens)}`;

  document.getElementById('uptime').textContent = formatDuration(s.uptime_s);

  // Error breakdown
  const ebEl = document.getElementById('error-breakdown');
  if (s.error_breakdown.length) {
    ebEl.innerHTML = s.error_breakdown.map(([code, count]) =>
      `<div style="display:flex;justify-content:space-between;padding:4px 0;font-family:var(--mono);font-size:12px">
        <span style="color:var(--red)">${code}</span><span style="color:var(--text-2)">${count}</span>
      </div>`
    ).join('');
  } else {
    ebEl.innerHTML = '<div style="color:var(--text-2);font-size:12px">No errors ✓</div>';
  }

  // Model breakdown
  const mbEl = document.getElementById('model-breakdown');
  if (s.model_breakdown.length) {
    mbEl.innerHTML = s.model_breakdown.map(([model, count]) =>
      `<div style="display:flex;justify-content:space-between;padding:4px 0;font-family:var(--mono);font-size:12px">
        <span style="color:var(--cyan)">${model}</span><span style="color:var(--text-2)">${count}</span>
      </div>`
    ).join('');
  } else {
    mbEl.innerHTML = EMPTY_MODELS_HTML;
  }

  updateCharts();
}

function resetOverview() {
  document.getElementById('overview-coverage').textContent = `Mode: ${currentOverviewMode}`;
  const scoreEl = document.getElementById('health-score');
  scoreEl.textContent = '—';
  scoreEl.style.color = 'var(--text-0)';

  document.getElementById('health-label').textContent = '—';
  const bar = document.getElementById('health-bar');
  bar.style.width = '0%';
  bar.style.background = 'transparent';

  document.getElementById('total-requests').textContent = '0';
  document.getElementById('requests-rate').textContent = '0 req/min';

  const srEl = document.getElementById('success-rate');
  srEl.textContent = '—';
  srEl.style.color = 'var(--text-0)';
  document.getElementById('error-count').textContent = '0 errors | 0 last 5min';

  const avgEl = document.getElementById('avg-ttft');
  avgEl.textContent = '—';
  avgEl.style.color = 'var(--text-0)';
  document.getElementById('ttft-p95').textContent = 'P95: — · P99: —';

  document.getElementById('median-ttft').textContent = '—';
  document.getElementById('worst-ttft').textContent = 'Worst: —';

  const stallEl = document.getElementById('total-stalls');
  stallEl.textContent = '0';
  stallEl.style.color = 'var(--green)';
  document.getElementById('stall-time').textContent = '0s lost';

  const toEl = document.getElementById('total-timeouts');
  toEl.textContent = '0';
  toEl.style.color = 'var(--green)';
  document.getElementById('conn-errors').textContent = '0 conn errors';

  document.getElementById('total-tokens').textContent = '0';
  document.getElementById('token-breakdown').textContent = 'in: 0 / out: 0';
  document.getElementById('uptime').textContent = '0:00:00';
  document.getElementById('error-breakdown').innerHTML = '<div style="color:var(--text-2);font-size:12px">No errors ✓</div>';
  document.getElementById('model-breakdown').innerHTML = EMPTY_MODELS_HTML;
}

async function loadOverview() {
  const token = ++overviewRequestToken;

  try {
    const resp = await fetch(`/api/stats?mode=${encodeURIComponent(currentOverviewMode)}`);
    const snapshot = await resp.json();
    if (token !== overviewRequestToken) return;
    statsSnapshot = snapshot;
    updateOverview();
  } catch (e) {
    console.error('Failed to load overview stats:', e);
  }
}

function scheduleOverviewRefresh() {
  if (overviewRefreshTimer) clearTimeout(overviewRefreshTimer);
  overviewRefreshTimer = setTimeout(() => {
    overviewRefreshTimer = null;
    loadOverview();
  }, 250);
}

function setOverviewMode(mode) {
  currentOverviewMode = normalizeOverviewMode(mode);
  persistOverviewMode(currentOverviewMode);
  document.getElementById('overview-live-btn').classList.toggle('active', currentOverviewMode === 'live');
  document.getElementById('overview-historical-btn').classList.toggle('active', currentOverviewMode === 'historical');
  resetOverview();
  resetCharts();
  loadOverview();
}

async function runResetAction({ buttonId, endpoint, confirmText, pendingText, successText, errorText }) {
  const button = document.getElementById(buttonId);
  if (button.disabled) return;

  const confirmed = window.confirm(confirmText);
  if (!confirmed) return;

  const originalText = button.textContent;
  button.disabled = true;
  button.textContent = pendingText;

  try {
    const resp = await fetch(endpoint, { method: 'POST' });
    if (!resp.ok) throw new Error(`HTTP ${resp.status}`);

    const result = handleReset(await resp.json());
    button.textContent = successText(result);
  } catch (e) {
    console.error(errorText, e);
    button.textContent = 'Clear failed';
  } finally {
    setTimeout(() => {
      button.disabled = false;
      button.textContent = originalText;
    }, 1800);
  }
}

function clearMemoryStats() {
  return runResetAction({
    buttonId: 'clear-memory-btn',
    endpoint: '/api/reset-memory',
    confirmText: 'Clear only in-memory dashboard stats? Saved SQLite data will be kept.',
    pendingText: 'Clearing…',
    successText: (result) => `Cleared ${result.cleared_entries} entr${result.cleared_entries === 1 ? 'y' : 'ies'}`,
    errorText: 'Failed to clear in-memory stats:'
  });
}

function clearAllData() {
  return runResetAction({
    buttonId: 'clear-data-btn',
    endpoint: '/api/reset',
    confirmText: 'Clear all saved SQLite request history and reset all dashboard statistics? This cannot be undone.',
    pendingText: 'Clearing…',
    successText: (result) => `Cleared ${result.deleted_persisted_entries} saved entr${result.deleted_persisted_entries === 1 ? 'y' : 'ies'}`,
    errorText: 'Failed to clear saved data and stats:'
  });
}

function handleReset(summary) {
  const mode = summary?.mode;
  const memoryOnly = mode === 'memory_only' || mode === 'MemoryOnly';

  statsSnapshot = null;
  entries = [];
  entriesRequestToken += 1;
  sessionsRequestToken += 1;
  sessionDetailsRequestToken += 1;
  sessionConversationFullTextCache.clear();

  resetOverview();
  resetCharts();
  rebuildTable();
  document.getElementById('result-count').textContent = '0 results';
  document.getElementById('session-list').innerHTML = EMPTY_SESSIONS_HTML;
  const sessionDetails = document.getElementById('session-details');
  if (sessionDetails) {
    sessionDetails.innerHTML = '<div class="card"><div class="empty-state" style="padding:20px 12px"><h3 style="font-size:14px">No session selected</h3><p>Click a session card to inspect summary, timeline, and graph details</p></div></div>';
  }
  document.getElementById('anomaly-list').innerHTML = EMPTY_ANOMALIES_HTML;
  _selectedConformanceModel = null;
  document.getElementById('conformance-detail').style.display = 'none';
  loadConformanceData();
  closeModal();
  loadOverview();

  if (memoryOnly) {
    loadEntries();
    loadSessions();
  }

  return summary;
}
