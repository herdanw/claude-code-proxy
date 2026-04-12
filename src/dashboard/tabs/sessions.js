// sessions.js — Sessions tab
let selectedSessionId = null;
let sessionsRequestToken = 0;
let sessionDetailsRequestToken = 0;
const sessionConversationFullTextCache = new Map();
const MAX_SESSION_CONVERSATION_CACHE_ITEMS = 10;
const EMPTY_SESSIONS_HTML = '<div class="empty-state"><h3>No sessions recorded yet</h3><p>Sessions appear as requests come in</p></div>';

function setSessionConversationCache(sessionId, payload) {{
  const key = String(sessionId);
  if (sessionConversationFullTextCache.has(key)) {{
    sessionConversationFullTextCache.delete(key);
  }}
  sessionConversationFullTextCache.set(key, payload);
  while (sessionConversationFullTextCache.size > MAX_SESSION_CONVERSATION_CACHE_ITEMS) {{
    const oldestKey = sessionConversationFullTextCache.keys().next().value;
    if (!oldestKey) break;
    sessionConversationFullTextCache.delete(oldestKey);
  }}
}}

async function loadSessions() {{
  const token = ++sessionsRequestToken;

  try {{
    const resp = await fetch('/api/sessions/merged');
    if (!resp.ok) throw new Error(`Sessions HTTP ${{resp.status}}`);

    const mergedRows = envelopeData(await resp.json());
    if (token !== sessionsRequestToken) return;

    const rows = (Array.isArray(mergedRows) ? mergedRows : []).map((row) => {{
      return {{
        session_id: row.session_id,
        proxy: row.presence === 'proxy' || row.presence === 'both' ? row : null,
        local: row.presence === 'local' || row.presence === 'both' ? row : null,
        proxyRequestCount: Number(row.proxy_request_count) || 0,
        localRequestCount: Number(row.local_event_count) || 0,
        proxyErrorCount: Number(row.proxy_error_count) || 0,
        proxyStallCount: Number(row.proxy_stall_count) || 0,
        lastActivityMs: Number(row.last_activity_ms) || 0,
      }};
    }}).sort((a, b) => b.lastActivityMs - a.lastActivityMs || String(a.session_id).localeCompare(String(b.session_id)));

    const container = document.getElementById('session-list');
    if (!rows.length) {{
      container.innerHTML = EMPTY_SESSIONS_HTML;
      const details = document.getElementById('session-details');
      if (details) {{
        details.innerHTML = '<div class="card"><div class="empty-state" style="padding:20px 12px"><h3 style="font-size:14px">No session selected</h3><p>Click a session card to inspect summary, timeline, and graph details</p></div></div>';
      }}
      return;
    }}

    container.innerHTML = rows.map((row) => {{
      const badges = `${{row.proxy ? '<span class="session-badge proxy">proxy</span>' : ''}} ${{row.local ? '<span class="session-badge local">local</span>' : ''}}`;
      const lastSeen = row.lastActivityMs > 0 ? new Date(row.lastActivityMs).toLocaleString() : 'Unknown';
      const projectHint = row.local?.project_path
        ? `<div style="color:var(--text-2);font-size:11px;word-break:break-all;margin-top:4px">${{esc(row.local.project_path)}}</div>`
        : '';

      return `
        <div class="session-card ${{selectedSessionId === row.session_id ? 'active' : ''}}" data-session-id="${{esc(row.session_id)}}" tabindex="0" style="margin-bottom:8px">
          <div style="display:flex;align-items:center;gap:8px;flex-wrap:wrap">
            <div style="font-weight:600;font-size:13px">${{esc(row.session_id)}}</div>
            ${{badges}}
            <div style="margin-left:auto;color:var(--text-2);font-size:11px">Last seen: ${{esc(lastSeen)}}</div>
          </div>
          <div class="session-meta" style="margin-top:6px">
            <span>Proxy req: ${{row.proxyRequestCount}}</span>
            <span>Local req: ${{row.localRequestCount}}</span>
            <span style="color:${{row.proxyErrorCount ? 'var(--red)' : 'var(--green)'}}">Errors: ${{row.proxyErrorCount}}</span>
            <span style="color:${{row.proxyStallCount ? 'var(--peach)' : 'var(--text-2)'}}">Stalls: ${{row.proxyStallCount}}</span>
          </div>
          ${{projectHint}}
        </div>
      `;
    }}).join('');

    container.querySelectorAll('.session-card[data-session-id]').forEach(card => {{
      const sid = card.dataset.sessionId;
      const open = () => selectSession(sid, rows);
      card.addEventListener('click', open);
      card.addEventListener('keydown', (ev) => {{
        if (ev.key === 'Enter') open();
      }});
    }});

    if (selectedSessionId && rows.some(r => r.session_id === selectedSessionId)) {{
      await selectSession(selectedSessionId, rows);
    }}

    await populateSessionFilter();
  }} catch (e) {{
    console.error('Failed to load sessions:', e);
  }}
}}

async function loadSessionDetails(sessionId, opts = {{}}) {{
  const token = ++sessionDetailsRequestToken;
  const limit = Number.isFinite(opts.limit) ? Math.max(1, Math.floor(opts.limit)) : 100;
  const includeFullText = opts.includeFullText === true;
  const projectPath = typeof opts.projectPath === 'string' && opts.projectPath.trim() ? opts.projectPath.trim() : null;

  const params = new URLSearchParams({{
    session_id: String(sessionId),
    limit: String(limit),
  }});
  if (includeFullText) params.set('include_full_text', 'true');
  if (projectPath) params.set('project_path', projectPath);

  const detailsResp = await fetch(`/api/session-details?${{params.toString()}}`);
  if (detailsResp.status === 404) {{
    if (token !== sessionDetailsRequestToken || selectedSessionId !== sessionId) return null;
    return {{ notFound: true }};
  }}
  if (!detailsResp.ok) throw new Error(`HTTP ${{detailsResp.status}}`);
  const payload = envelopeData(await detailsResp.json());

  if (token !== sessionDetailsRequestToken || selectedSessionId !== sessionId) return null;
  return payload;
}}

function mergeConversationWithFullText(previewConversation, fullConversation) {{
  const merged = Array.isArray(previewConversation)
    ? previewConversation.map(item => (item && typeof item === 'object') ? {{ ...item }} : item)
    : [];

  if (!Array.isArray(fullConversation) || !fullConversation.length) {{
    return merged;
  }}

  const fullItems = fullConversation.filter(item => item && typeof item === 'object');
  const usedPreview = new Set();
  const result = [];

  const isMatch = (previewItem, fullItem) => {{
    if (!previewItem || typeof previewItem !== 'object' || !fullItem || typeof fullItem !== 'object') {{
      return false;
    }}

    if (previewItem.id != null && fullItem.id != null) {{
      return String(previewItem.id) === String(fullItem.id);
    }}

    return Number(previewItem.timestamp_ms) === Number(fullItem.timestamp_ms)
      && String(previewItem.role ?? '') === String(fullItem.role ?? '')
      && String(previewItem.preview ?? '') === String(fullItem.preview ?? '');
  }};

  for (const fullItem of fullItems) {{
    let matchedPreviewIndex = -1;
    for (let index = 0; index < merged.length; index += 1) {{
      if (usedPreview.has(index)) continue;
      if (isMatch(merged[index], fullItem)) {{
        matchedPreviewIndex = index;
        break;
      }}
    }}

    if (matchedPreviewIndex >= 0) {{
      usedPreview.add(matchedPreviewIndex);
      result.push({{ ...merged[matchedPreviewIndex], ...fullItem }});
    }} else {{
      result.push({{ ...fullItem }});
    }}
  }}

  for (let index = 0; index < merged.length; index += 1) {{
    if (!usedPreview.has(index) && merged[index] && typeof merged[index] === 'object') {{
      result.push({{ ...merged[index] }});
    }}
  }}

  return result;
}}

function renderSessionConversationItem(message, sessionId) {{
  const role = (message && typeof message.role === 'string' && message.role.trim())
    ? message.role.trim()
    : 'unknown';
  const timestampMs = Number(message?.timestamp_ms);
  const when = Number.isFinite(timestampMs) && timestampMs > 0
    ? new Date(timestampMs).toLocaleString()
    : 'Unknown time';

  const previewText = typeof message?.preview === 'string'
    ? message.preview
    : (typeof message?.full_text === 'string' ? message.full_text : '');
  const fullText = typeof message?.full_text === 'string' ? message.full_text : null;

  const isExpandable = message?.expandable === true;
  const messageId = message?.id != null ? String(message.id) : `${{sessionId}}-${{role}}-${{timestampMs || 0}}`;

  const isTextTruncated = message?.text_truncated === true;
  const isPreviewOnly = fullText == null && typeof message?.preview === 'string';
  const truncatedFlags = [];
  if (isTextTruncated) truncatedFlags.push('text');

  const allowLazyLoad = fullText == null && (isTextTruncated || isPreviewOnly);

  const indicatorParts = [];
  if ((isExpandable || allowLazyLoad) && !fullText) {{
    indicatorParts.push('<span style="color:var(--text-2)">Preview only</span>');
  }}
  if (truncatedFlags.length > 0) {{
    indicatorParts.push(`<span style="color:var(--peach)">Truncated ${{truncatedFlags.join(' + ')}}</span>`);
  }}
  const indicator = indicatorParts.length
    ? `<div class="correlation-meta" style="margin-top:4px">${{indicatorParts.join(' · ')}}</div>`
    : '';

  const canLoadFullText = fullText == null && (isExpandable || allowLazyLoad);
  const expandButton = canLoadFullText
    ? `<button class="filter-btn" type="button" data-conversation-expand="${{esc(messageId)}}">Load full text</button>`
    : '';

  const previewBlock = `<div style="color:var(--text-2);font-size:11px;margin-top:8px">Preview</div><pre style="margin-top:4px;white-space:pre-wrap;word-break:break-word;font-family:var(--mono);font-size:12px">${{esc(previewText || '—')}}</pre>`;
  const fullTextBlock = fullText != null
    ? `<div style="color:var(--text-2);font-size:11px;margin-top:8px">Full text</div><pre style="margin-top:4px;white-space:pre-wrap;word-break:break-word;font-family:var(--mono);font-size:12px">${{esc(fullText || '—')}}</pre>`
    : '';

  return `
    <div class="correlation-item" data-conversation-item="${{esc(messageId)}}">
      <div class="correlation-header" style="align-items:flex-start;gap:8px">
        <div>
          <strong>${{esc(role)}}</strong>
          <div style="color:var(--text-2);font-size:11px">${{esc(when)}}</div>
          ${{indicator}}
        </div>
        ${{expandButton}}
      </div>
      ${{previewBlock}}
      ${{fullTextBlock}}
    </div>
  `;
}}

function renderSessionConversation(conversationRoot, sessionId, detailsPayload) {{
  if (!conversationRoot) return;

  const conversation = Array.isArray(detailsPayload?.conversation) ? detailsPayload.conversation : [];
  const sectionFlags = detailsPayload?.truncated_sections;
  const sectionHints = [];
  if (sectionFlags && typeof sectionFlags === 'object') {{
    if (sectionFlags.conversation === true) sectionHints.push('conversation list truncated');
    if (sectionFlags.conversation_preview === true) sectionHints.push('conversation preview truncated');
    if (sectionFlags.conversation_full_text === true) sectionHints.push('conversation full text truncated');
  }}

  if (!conversation.length) {{
    const hint = sectionHints.length
      ? `<div class="correlation-meta" style="margin-top:6px"><span style="color:var(--peach)">${{esc(sectionHints.join(' · '))}}</span></div>`
      : '';
    conversationRoot.innerHTML = `<div style="color:var(--text-2);font-size:12px">No conversation messages for this session.</div>${{hint}}`;
    return;
  }}

  const listHtml = conversation.map(message => renderSessionConversationItem(message, sessionId)).join('');
  const hintsHtml = sectionHints.length
    ? `<div class="correlation-meta" style="margin-top:8px"><span style="color:var(--peach)">${{esc(sectionHints.join(' · '))}}</span></div>`
    : '';
  conversationRoot.innerHTML = `${{listHtml}}${{hintsHtml}}`;

  conversationRoot.querySelectorAll('[data-conversation-expand]').forEach(button => {{
    button.addEventListener('click', () => expandSessionConversationMessage(sessionId));
  }});
}}

async function expandSessionConversationMessage(sessionId) {{
  if (!sessionId) return;

  const cacheKey = String(sessionId);
  let cached = sessionConversationFullTextCache.get(cacheKey);
  const hasCachedFullText = Array.isArray(cached?.conversation)
    && cached.conversation.some(item => typeof item?.full_text === 'string');

  if (!hasCachedFullText) {{
    const projectPath = cached?.selected_project_path
      || (Array.isArray(cached?.project_paths) && cached.project_paths.length ? cached.project_paths[0] : null)
      || null;
    const fullDetails = await loadSessionDetails(sessionId, {{ limit: 100, includeFullText: true, projectPath }});
    if (!fullDetails) return;

    const preview = sessionConversationFullTextCache.get(cacheKey);
    const mergedConversation = mergeConversationWithFullText(preview?.conversation, fullDetails.conversation);
    cached = {{
      ...preview,
      ...fullDetails,
      conversation: mergedConversation,
      truncated_sections: {{
        ...(preview?.truncated_sections || {{}}),
        ...(fullDetails.truncated_sections || {{}}),
      }},
    }};
    setSessionConversationCache(cacheKey, cached);
  }}

  const conversationRoot = document.getElementById('session-detail-conversation');
  if (!conversationRoot || selectedSessionId !== sessionId) return;
  renderSessionConversation(conversationRoot, sessionId, cached);
}}

async function selectSession(sessionId, knownRows = null) {{
  selectedSessionId = sessionId;
  const rows = knownRows || [];
  const fallbackSession = rows.find(r => r.session_id === sessionId) || {{ session_id: sessionId, proxy: null, local: null }};

  document.querySelectorAll('#session-list .session-card').forEach(card => {{
    card.classList.toggle('active', card.dataset.sessionId === sessionId);
  }});

  const details = document.getElementById('session-details');
  if (!details) return;

  details.innerHTML = `
    <div class="card" style="margin-bottom:12px">
      <div class="correlation-header">
        <h3 style="font-size:12px;color:var(--text-2);text-transform:uppercase;letter-spacing:0.05em">Session Summary</h3>
        <button class="filter-btn" id="session-filter-requests-btn" type="button">Filter requests tab</button>
      </div>
      <div class="modal-grid" style="margin-top:12px">
        <div class="modal-stat"><div class="modal-stat-label">Session</div><div class="modal-stat-value" id="session-summary-session-id">${{esc(sessionId)}}</div></div>
        <div class="modal-stat"><div class="modal-stat-label">Proxy requests</div><div class="modal-stat-value" id="session-summary-proxy-requests">${{fallbackSession.proxy?.request_count || 0}}</div></div>
        <div class="modal-stat"><div class="modal-stat-label">Local requests</div><div class="modal-stat-value" id="session-summary-local-requests">${{fallbackSession.local?.request_count || 0}}</div></div>
        <div class="modal-stat"><div class="modal-stat-label">Errors</div><div class="modal-stat-value" id="session-summary-errors">${{fallbackSession.proxy?.error_count || 0}}</div></div>
      </div>
    </div>

    <div class="card correlation-panel" style="margin-bottom:12px">
      <div class="correlation-header">
        <h3 style="font-size:12px;color:var(--text-2);text-transform:uppercase;letter-spacing:0.05em">Requests</h3>
      </div>
      <div id="session-detail-requests" style="margin-top:10px;color:var(--text-2);font-size:12px">Loading session requests…</div>
    </div>

    <div class="card correlation-panel" style="margin-bottom:12px">
      <div class="correlation-header">
        <h3 style="font-size:12px;color:var(--text-2);text-transform:uppercase;letter-spacing:0.05em">Conversation Preview</h3>
      </div>
      <div id="session-detail-conversation" style="margin-top:10px;color:var(--text-2);font-size:12px">Loading conversation preview…</div>
    </div>

    <div class="card correlation-panel" style="margin-bottom:12px">
      <div class="correlation-header">
        <h3 style="font-size:12px;color:var(--text-2);text-transform:uppercase;letter-spacing:0.05em">Timeline Summary</h3>
      </div>
      <div id="session-detail-timeline" style="margin-top:10px;color:var(--text-2);font-size:12px">Loading timeline summary…</div>
    </div>

    <div class="card correlation-panel" id="session-graph">
      <div class="correlation-header">
        <h3 style="font-size:12px;color:var(--text-2);text-transform:uppercase;letter-spacing:0.05em">Session Graph Summary</h3>
      </div>
      <div id="session-graph-list" style="margin-top:10px;color:var(--text-2);font-size:12px">Loading graph summary…</div>
    </div>
  `;

  document.getElementById('session-filter-requests-btn')?.addEventListener('click', () => {{
    filterSession(sessionId);
  }});

  try {{
    const preferredProjectPath = fallbackSession.local?.project_path
      || (Array.isArray(fallbackSession.local?.project_paths) && fallbackSession.local.project_paths.length ? fallbackSession.local.project_paths[0] : null)
      || null;
    let sessionDetails = await loadSessionDetails(sessionId, {{ limit: 100, projectPath: preferredProjectPath }});
    if (!sessionDetails) return;

    if (sessionDetails.notFound && preferredProjectPath) {{
      sessionDetails = await loadSessionDetails(sessionId, {{ limit: 100 }});
      if (!sessionDetails) return;
    }}

    if (sessionDetails.notFound) {{
      const reqRoot = document.getElementById('session-detail-requests');
      if (reqRoot) reqRoot.innerHTML = '<div style="color:var(--text-2);font-size:12px">Session not found.</div>';
      const conversationRoot = document.getElementById('session-detail-conversation');
      if (conversationRoot) conversationRoot.innerHTML = '<div style="color:var(--text-2);font-size:12px">No conversation messages for this session.</div>';
      const timelineRoot = document.getElementById('session-detail-timeline');
      if (timelineRoot) timelineRoot.innerHTML = '<div style="color:var(--text-2);font-size:12px">No timeline data for this session.</div>';
      const graphRoot = document.getElementById('session-graph-list');
      if (graphRoot) graphRoot.innerHTML = '<div style="color:var(--text-2);font-size:12px">No graph data for this session.</div>';
      return;
    }}

    const summary = sessionDetails.summary || {{}};
    const requests = Array.isArray(sessionDetails.requests) ? sessionDetails.requests : [];
    const timelineItems = Array.isArray(sessionDetails.timeline) ? sessionDetails.timeline : [];

    const cacheKey = String(sessionId);
    const cached = sessionDetails;
    setSessionConversationCache(cacheKey, cached);

    const summarySession = document.getElementById('session-summary-session-id');
    const summaryProxyRequests = document.getElementById('session-summary-proxy-requests');
    const summaryLocalRequests = document.getElementById('session-summary-local-requests');
    const summaryErrors = document.getElementById('session-summary-errors');

    if (summarySession) summarySession.textContent = sessionDetails.session_id || sessionId;
    if (summaryProxyRequests) summaryProxyRequests.textContent = String(summary.proxy_request_count_total ?? fallbackSession.proxyRequestCount ?? fallbackSession.proxy?.request_count ?? 0);
    if (summaryLocalRequests) summaryLocalRequests.textContent = String(summary.conversation_message_count_total ?? fallbackSession.localRequestCount ?? fallbackSession.local?.request_count ?? 0);
    if (summaryErrors) summaryErrors.textContent = String(summary.proxy_error_count_total ?? fallbackSession.proxyErrorCount ?? fallbackSession.proxy?.error_count ?? 0);

    const reqRoot = document.getElementById('session-detail-requests');
    if (reqRoot) {{
      if (!requests.length) {{
        reqRoot.innerHTML = '<div style="color:var(--text-2);font-size:12px">No requests for this session.</div>';
      }} else {{
        reqRoot.innerHTML = `
          <table>
            <thead><tr><th>Time</th><th>Status</th><th>TTFT</th><th>Duration</th><th>Path</th></tr></thead>
            <tbody>
              ${{requests.slice(0, 20).map(r => {{
                const timestampMs = Number.isFinite(r.timestamp_ms) ? r.timestamp_ms : Date.parse(r.timestamp || '') || 0;
                const statusText = typeof r.status === 'string'
                  ? r.status
                  : (r.status_label || r.status_code || r.status?.value || r.status?.type || '?');
                const ttftMs = Number(r.ttft_ms);
                const durationMs = Number(r.duration_ms);
                const path = r.path || r.request_path || '—';
                return `<tr><td>${{timestampMs ? new Date(timestampMs).toLocaleTimeString() : '—'}}</td><td>${{esc(statusText)}}</td><td>${{Number.isFinite(ttftMs) ? (ttftMs / 1000).toFixed(2) + 's' : '—'}}</td><td>${{Number.isFinite(durationMs) ? (durationMs / 1000).toFixed(2) + 's' : '—'}}</td><td style="max-width:280px;overflow:hidden;text-overflow:ellipsis" title="${{esc(path)}}">${{esc(path)}}</td></tr>`;
              }}).join('')}}
            </tbody>
          </table>
        `;
      }}
    }}

    const conversationRoot = document.getElementById('session-detail-conversation');
    if (conversationRoot) {{
      renderSessionConversation(conversationRoot, sessionId, sessionConversationFullTextCache.get(String(sessionId)));
    }}

    const timelineRoot = document.getElementById('session-detail-timeline');
    if (timelineRoot) {{
      if (!timelineItems.length) {{
        timelineRoot.innerHTML = '<div style="color:var(--text-2);font-size:12px">No timeline items for this session.</div>';
      }} else {{
        const requestEvents = timelineItems.filter(i => i.kind === 'request').length;
        const localEvents = timelineItems.filter(i => i.kind === 'local_event').length;
        timelineRoot.innerHTML = `<div class="correlation-item"><div class="correlation-meta"><span>Total items: ${{timelineItems.length}}</span><span>Requests: ${{requestEvents}}</span><span>Local events: ${{localEvents}}</span></div><div>Aggregated from session details.</div></div>`;
      }}
    }}

    const graphRoot = document.getElementById('session-graph-list');
    if (graphRoot) {{
      const requestCount = Number(summary.proxy_request_count_total) || requests.length;
      const eventCount = Number(summary.local_event_count_total) || timelineItems.filter(i => i.kind === 'local_event').length;
      const messageCount = Number(summary.conversation_message_count_total) || 0;
      graphRoot.innerHTML = `<div class="correlation-item"><div class="correlation-meta"><span>Requests: ${{requestCount}}</span><span>Events: ${{eventCount}}</span><span>Messages: ${{messageCount}}</span></div><div>Session graph summary derived from aggregated session details.</div></div>`;
    }}
  }} catch (e) {{
    console.error('Failed to render session details:', e);
  }}
}}
