// Conformance tab — model scoreboard

function initConformanceTab() {
  loadConformanceData();
}

async function loadConformanceData() {
  const scoreboard = document.getElementById('conformance-scoreboard');
  const notes = document.getElementById('conformance-notes');

  try {
    const res = await fetch('/api/model-config');
    const data = await res.json();
    const models = data.models || [];

    if (models.length === 0) {
      scoreboard.innerHTML = '<div class="empty-state" style="padding:20px 12px"><h3 style="font-size:14px">No model data yet</h3><p>Model statistics appear after requests are proxied and analyzed</p></div>';
      return;
    }

    let html = '<table><thead><tr><th>Model</th><th>Requests</th><th>Avg TTFT</th><th>Errors</th><th>Error Rate</th><th>Status</th></tr></thead><tbody>';

    for (const m of models) {
      const errorRate = m.request_count > 0 ? ((m.error_count / m.request_count) * 100).toFixed(1) : '0.0';
      const avgTtft = m.avg_ttft_ms != null ? m.avg_ttft_ms.toFixed(0) + 'ms' : '\u2014';
      const lastSeen = m.last_seen_ms ? new Date(m.last_seen_ms).toLocaleTimeString() : '\u2014';

      let sampleCount = 0;
      let profileStatus = 'collecting';
      try {
        const profileRes = await fetch('/api/models/' + encodeURIComponent(m.model) + '/profile');
        const profile = await profileRes.json();
        sampleCount = profile.sample_count || 0;
        if (sampleCount >= 50) profileStatus = 'profiled';
      } catch (e) { /* ignore */ }

      const statusBadge = profileStatus === 'profiled'
        ? '<span style="color:var(--green);font-size:11px">\u25CF Profiled</span>'
        : '<span style="color:var(--yellow);font-size:11px">\u25CF Collecting (' + sampleCount + '/50)</span>';

      const errorRateColor = parseFloat(errorRate) > 10 ? 'var(--red)' : parseFloat(errorRate) > 5 ? 'var(--yellow)' : 'var(--green)';

      html += '<tr>';
      html += '<td style="font-family:var(--mono);font-size:12px">' + esc(m.model) + '</td>';
      html += '<td>' + m.request_count + '</td>';
      html += '<td>' + avgTtft + '</td>';
      html += '<td>' + m.error_count + '</td>';
      html += '<td style="color:' + errorRateColor + '">' + errorRate + '%</td>';
      html += '<td>' + statusBadge + '</td>';
      html += '</tr>';
    }

    html += '</tbody></table>';
    scoreboard.innerHTML = html;

    const totalRequests = models.reduce((s, m) => s + m.request_count, 0);
    const totalErrors = models.reduce((s, m) => s + m.error_count, 0);
    const overallErrorRate = totalRequests > 0 ? ((totalErrors / totalRequests) * 100).toFixed(1) : '0.0';
    notes.innerHTML = '<div style="font-size:12px;color:var(--text-2);line-height:1.6">'
      + '<p><strong>' + models.length + '</strong> model(s) observed across <strong>' + totalRequests + '</strong> requests.</p>'
      + '<p>Overall error rate: <strong>' + overallErrorRate + '%</strong></p>'
      + '<p style="margin-top:8px;font-size:11px">Models are automatically profiled after 50 analyzed requests. Profiled models show observed TTFT averages and error rates.</p>'
      + '</div>';

  } catch (err) {
    scoreboard.innerHTML = '<div class="empty-state" style="padding:20px 12px"><h3 style="font-size:14px">Failed to load</h3><p>' + esc(err.message) + '</p></div>';
  }
}
