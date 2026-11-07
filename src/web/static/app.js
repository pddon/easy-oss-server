// OSS Server Admin - 前端单页应用

const API_BASE = '/api/admin';
let currentRoute = 'apps';
let apps = [];
let buckets = [];
let currentAppId = null;
let currentBucket = null;
let fileMarker = '';
let filePrefix = '';

// ---- 工具函数 ----

function getAuthHeaders() {
    const token = localStorage.getItem('admin_token') || '';
    return {
        'X-Admin-Token': token,
        'Content-Type': 'application/json',
    };
}

async function apiRequest(path, options = {}) {
    const url = API_BASE + path;
    const headers = { ...getAuthHeaders(), ...(options.headers || {}) };
    const res = await fetch(url, { ...options, headers });

    if (res.status === 401) {
        logout();
        throw new Error('未登录');
    }

    if (!res.ok) {
        let msg = '请求失败';
        try {
            const data = await res.json();
            msg = data.message || msg;
        } catch (_) { }
        throw new Error(msg);
    }

    if (res.status === 204) return null;
    return res.json();
}

function formatSize(bytes) {
    if (bytes === 0) return '0 B';
    const k = 1024;
    const sizes = ['B', 'KB', 'MB', 'GB', 'TB'];
    const i = Math.floor(Math.log(bytes) / Math.log(k));
    return parseFloat((bytes / Math.pow(k, i)).toFixed(2)) + ' ' + sizes[i];
}

function formatDate(timestamp) {
    const d = new Date(timestamp * 1000);
    return d.toLocaleString('zh-CN');
}

function showToast(message, type = 'info') {
    const toast = document.getElementById('toast');
    toast.textContent = message;
    toast.className = `toast ${type}`;
    toast.classList.remove('hidden');
    setTimeout(() => toast.classList.add('hidden'), 3000);
}

function openModal(title, bodyHtml) {
    document.getElementById('modal-title').textContent = title;
    document.getElementById('modal-body').innerHTML = bodyHtml;
    document.getElementById('modal').classList.remove('hidden');
}

function closeModal() {
    document.getElementById('modal').classList.add('hidden');
}

// ---- 登录/登出 ----

function login(token) {
    localStorage.setItem('admin_token', token);
    document.getElementById('login-page').classList.add('hidden');
    document.getElementById('main-page').classList.remove('hidden');
    showToast('登录成功', 'success');
    handleRoute();
}

function logout() {
    localStorage.removeItem('admin_token');
    document.getElementById('main-page').classList.add('hidden');
    document.getElementById('login-page').classList.remove('hidden');
    document.getElementById('login-error').textContent = '';
}

function checkAuth() {
    const token = localStorage.getItem('admin_token');
    if (token) {
        document.getElementById('login-page').classList.add('hidden');
        document.getElementById('main-page').classList.remove('hidden');
        return true;
    }
    return false;
}

// ---- 路由 ----

function handleRoute() {
    const hash = window.location.hash.slice(2) || 'apps'; // Remove #/
    const route = hash.split('/')[0] || 'apps';

    currentRoute = route;

    // Update nav
    document.querySelectorAll('.nav-item').forEach(item => {
        item.classList.remove('active');
        if (item.dataset.route === route) {
            item.classList.add('active');
        }
    });

    // Hide all views
    document.querySelectorAll('.view').forEach(v => v.classList.add('hidden'));

    // Show current view
    const viewId = `view-${route}`;
    const viewEl = document.getElementById(viewId);
    if (viewEl) {
        viewEl.classList.remove('hidden');
    }

    // Update title
    const titles = {
        apps: '应用管理',
        buckets: '存储桶',
        files: '文件浏览',
        trash: '回收站',
        maintenance: '清理与自检',
        stats: '使用统计',
    };
    document.getElementById('page-title').textContent = titles[route] || '管理后台';

    // Load data
    switch (route) {
        case 'apps':
            loadApps();
            break;
        case 'buckets':
            loadAppsForSelect();
            break;
        case 'files':
            loadBucketsForFileSelect();
            break;
        case 'trash':
            loadTrashFilters();
            break;
        case 'maintenance':
            loadMaintenance();
            break;
        case 'stats':
            loadStats();
            break;
    }
}

// ---- 应用管理 ----

let appsOffset = 0;
const APPS_LIMIT = 20;

async function loadApps() {
    try {
        const data = await apiRequest(`/apps?offset=${appsOffset}&limit=${APPS_LIMIT}`);
        apps = data.items;
        renderApps(data);
    } catch (e) {
        showToast(e.message, 'error');
    }
}

function renderApps(data) {
    const tbody = document.getElementById('apps-tbody');
    tbody.innerHTML = data.items.map(app => `
        <tr>
            <td><strong>${escapeHtml(app.name)}</strong></td>
            <td><code style="font-size:12px">${escapeHtml(app.app_id)}</code></td>
            <td>${formatDate(app.created_at)}</td>
            <td class="action-btns">
                <button class="btn btn-sm btn-secondary" onclick="showS3Credentials('${escapeHtml(app.app_id)}', '${escapeHtml(app.name)}')">S3 凭证</button>
                <button class="btn btn-sm btn-secondary" onclick="resetAppSecret('${escapeHtml(app.app_id)}', '${escapeHtml(app.name)}')">重置密钥</button>
                <button class="btn btn-sm btn-danger" onclick="deleteApp('${escapeHtml(app.app_id)}', '${escapeHtml(app.name)}')">删除</button>
            </td>
        </tr>
    `).join('');

    renderPagination('apps-pagination', data, (offset) => {
        appsOffset = offset;
        loadApps();
    });
}

document.getElementById('create-app-btn').addEventListener('click', () => {
    openModal('创建应用', `
        <div class="form-group">
            <label>应用名称</label>
            <input type="text" id="new-app-name" placeholder="请输入应用名称">
        </div>
        <div class="form-actions">
            <button class="btn btn-secondary" onclick="closeModal()">取消</button>
            <button class="btn btn-primary" onclick="submitCreateApp()">创建</button>
        </div>
    `);
});

async function submitCreateApp() {
    const name = document.getElementById('new-app-name').value.trim();
    if (!name) {
        showToast('请输入应用名称', 'error');
        return;
    }

    try {
        const data = await apiRequest('/apps', {
            method: 'POST',
            body: JSON.stringify({ name }),
        });
        closeModal();
        showToast('应用创建成功', 'success');
        // Show the secret
        openModal('应用已创建', `
            <p style="margin-bottom:12px">请妥善保存以下凭证，只显示一次：</p>
            <div class="form-group">
                <label>App ID</label>
                <input type="text" value="${escapeHtml(data.app_id)}" readonly>
            </div>
            <div class="form-group">
                <label>App Secret</label>
                <input type="text" value="${escapeHtml(data.app_secret)}" readonly>
            </div>
            <div class="form-actions">
                <button class="btn btn-primary" onclick="closeModal()">我知道了</button>
            </div>
        `);
        loadApps();
    } catch (e) {
        showToast(e.message, 'error');
    }
}

async function copyText(text) {
    try {
        if (navigator.clipboard && window.isSecureContext) {
            await navigator.clipboard.writeText(text);
        } else {
            const el = document.createElement('textarea');
            el.value = text;
            el.style.position = 'fixed';
            el.style.opacity = '0';
            document.body.appendChild(el);
            el.select();
            document.execCommand('copy');
            document.body.removeChild(el);
        }
        showToast('已复制', 'success');
    } catch (e) {
        showToast('复制失败，请手动选择文本', 'error');
    }
}

// S3 凭证：只展示 AccessKeyId / endpoint / path-style 示例。
// Secret 不在页面重复展示（与 App Secret 相同，需要时通过“重置密钥”获取）。
function showS3Credentials(appId, appName) {
    const origin = window.location.origin || 'http://localhost:8080';
    const pathExample = `${origin}/<bucket>/path/to/file.txt`;
    openModal(`S3 凭证 - ${appName}`, `
        <p style="margin-bottom:12px">S3 兼容接口沿用应用的 App ID / App Secret 作为 AccessKey / SecretKey，仅支持 path-style 寻址。</p>
        <div class="form-group">
            <label>AccessKeyId（= App ID）</label>
            <div style="display:flex;gap:8px">
                <input type="text" value="${escapeHtml(appId)}" readonly>
                <button class="btn btn-sm btn-secondary" onclick="copyText('${escapeHtml(appId)}')">复制</button>
            </div>
        </div>
        <div class="form-group">
            <label>SecretAccessKey</label>
            <input type="text" value="与 App Secret 相同" readonly>
            <p style="margin-top:6px;color:var(--text-secondary,#888)">
                S3 Secret 与 App Secret 完全相同，本页面不重复展示明文；需要获取或更换时请使用
                “重置密钥”（旧密钥会立即失效）。
            </p>
        </div>
        <div class="form-group">
            <label>S3 Endpoint</label>
            <div style="display:flex;gap:8px">
                <input type="text" value="${escapeHtml(origin)}" readonly>
                <button class="btn btn-sm btn-secondary" onclick="copyText('${escapeHtml(origin)}')">复制</button>
            </div>
        </div>
        <div class="form-group">
            <label>path-style 访问示例</label>
            <input type="text" value="${escapeHtml(pathExample)}" readonly>
            <p style="margin-top:6px;color:var(--text-secondary,#888)">
                Region 建议填 <code>us-east-1</code>；客户端需开启 path-style
                （如 boto3 的 <code>addressing_style: "path"</code>）。完整协议见
                <code>docs/s3-compatible-api.md</code>。
            </p>
        </div>
        <div class="form-actions">
            <button class="btn btn-primary" onclick="closeModal()">关闭</button>
        </div>
    `);
}

async function resetAppSecret(appId, appName) {
    if (!confirm(`确定要重置应用 "${appName}" 的密钥吗？旧密钥将立即失效。`)) return;

    try {
        const data = await apiRequest(`/apps/${appId}/reset-secret`, {
            method: 'POST',
        });
        openModal(`密钥已重置 - ${appName}`, `
            <p style="margin-bottom:12px">新密钥（请妥善保存）：</p>
            <div class="form-group">
                <label>App Secret</label>
                <input type="text" value="${escapeHtml(data.app_secret)}" readonly>
            </div>
            <div class="form-actions">
                <button class="btn btn-primary" onclick="closeModal()">关闭</button>
            </div>
        `);
        showToast('密钥重置成功', 'success');
    } catch (e) {
        showToast(e.message, 'error');
    }
}

async function deleteApp(appId, appName) {
    if (!confirm(`确定要删除应用 "${appName}" 吗？其下所有存储桶与文件将移入回收站（文件不会立即删除），可在回收站还原或彻底删除。`)) return;

    try {
        await apiRequest(`/apps/${appId}`, { method: 'DELETE' });
        showToast('应用已删除', 'success');
        loadApps();
    } catch (e) {
        showToast(e.message, 'error');
    }
}

// ---- 存储桶 ----

let bucketsOffset = 0;
const BUCKETS_LIMIT = 20;

async function loadAppsForSelect() {
    try {
        const data = await apiRequest('/apps?limit=100');
        apps = data.items;
        const select = document.getElementById('bucket-app-select');
        select.innerHTML = '<option value="">选择应用</option>' +
            apps.map(a => `<option value="${escapeHtml(a.app_id)}">${escapeHtml(a.name)}</option>`).join('');

        if (currentAppId && apps.some(a => a.app_id === currentAppId)) {
            select.value = currentAppId;
            loadBuckets();
        }
    } catch (e) {
        showToast(e.message, 'error');
    }
}

document.getElementById('bucket-app-select').addEventListener('change', (e) => {
    currentAppId = e.target.value;
    bucketsOffset = 0;
    if (currentAppId) {
        loadBuckets();
    } else {
        document.getElementById('buckets-tbody').innerHTML = '';
    }
});

async function loadBuckets() {
    if (!currentAppId) return;

    try {
        const data = await apiRequest(`/apps/${currentAppId}/buckets?offset=${bucketsOffset}&limit=${BUCKETS_LIMIT}`);
        renderBuckets(data);
    } catch (e) {
        showToast(e.message, 'error');
    }
}

function renderBuckets(data) {
    const tbody = document.getElementById('buckets-tbody');
    tbody.innerHTML = '加载中...';

    // Load stats for each bucket
    const itemsWithStats = data.items.map(async bucket => {
        // We'll fetch file count and size via object list
        // For simplicity, show basic info
        return {
            ...bucket,
            fileCount: '-',
            totalSize: '-',
        };
    });

    Promise.all(itemsWithStats).then(items => {
        tbody.innerHTML = items.map(b => `
            <tr>
                <td><strong>${escapeHtml(b.name)}</strong></td>
                <td><span class="badge ${b.is_public ? 'badge-public' : 'badge-private'}">${b.is_public ? '公开读' : '私有'}</span></td>
                <td>${b.fileCount}</td>
                <td>${b.totalSize}</td>
                <td>${formatDate(b.created_at)}</td>
                <td class="action-btns">
                    <button class="btn btn-sm btn-secondary" onclick="toggleBucketVisibility('${escapeHtml(b.name)}', ${b.is_public})">
                        ${b.is_public ? '设为私有' : '设为公开'}
                    </button>
                    <button class="btn btn-sm btn-danger" onclick="deleteBucket('${escapeHtml(b.name)}')">删除</button>
                </td>
            </tr>
        `).join('');
    });

    renderPagination('buckets-pagination', data, (offset) => {
        bucketsOffset = offset;
        loadBuckets();
    });
}

document.getElementById('create-bucket-btn').addEventListener('click', () => {
    if (!currentAppId) {
        showToast('请先选择应用', 'error');
        return;
    }

    openModal('创建存储桶', `
        <div class="form-group">
            <label>桶名称（3-63位小写字母、数字、短横线）</label>
            <input type="text" id="new-bucket-name" placeholder="my-bucket-name">
        </div>
        <div class="form-group">
            <label>访问权限</label>
            <select id="new-bucket-public">
                <option value="false">私有（默认）</option>
                <option value="true">公开读</option>
            </select>
        </div>
        <div class="form-actions">
            <button class="btn btn-secondary" onclick="closeModal()">取消</button>
            <button class="btn btn-primary" onclick="submitCreateBucket()">创建</button>
        </div>
    `);
});

async function submitCreateBucket() {
    const name = document.getElementById('new-bucket-name').value.trim();
    const isPublic = document.getElementById('new-bucket-public').value === 'true';

    if (!name) {
        showToast('请输入桶名称', 'error');
        return;
    }

    // Note: bucket creation uses OSS API, not admin API. 
    // For admin UI simplicity, we use the bucket creation via the app's credentials.
    // Since admin token doesn't give us direct bucket creation, let's add an admin endpoint.
    // For now, let's create a helper endpoint.

    // Actually, let's use the admin apps/buckets endpoint pattern.
    // We'll add a POST to /api/admin/apps/:appId/buckets
    try {
        await apiRequest(`/apps/${currentAppId}/buckets`, {
            method: 'POST',
            body: JSON.stringify({ name, is_public: isPublic }),
        });
        closeModal();
        showToast('存储桶创建成功', 'success');
        loadBuckets();
    } catch (e) {
        showToast(e.message, 'error');
    }
}

async function toggleBucketVisibility(bucketName, currentPublic) {
    // We need an admin endpoint for this. Let's use PUT /api/admin/apps/:appId/buckets/:name
    const newPublic = !currentPublic;
    try {
        await apiRequest(`/apps/${currentAppId}/buckets/${bucketName}`, {
            method: 'PUT',
            body: JSON.stringify({ is_public: newPublic }),
        });
        showToast(`已设为${newPublic ? '公开读' : '私有'}`, 'success');
        loadBuckets();
    } catch (e) {
        showToast(e.message, 'error');
    }
}

async function deleteBucket(bucketName) {
    if (!confirm(`确定要删除存储桶 "${bucketName}" 吗？请确保桶内已无文件。`)) return;

    try {
        await apiRequest(`/apps/${currentAppId}/buckets/${bucketName}`, {
            method: 'DELETE',
        });
        showToast('存储桶已删除', 'success');
        loadBuckets();
    } catch (e) {
        showToast(e.message, 'error');
    }
}

// ---- 文件浏览 ----

async function loadBucketsForFileSelect() {
    try {
        // Get all apps first, then all buckets
        const appsData = await apiRequest('/apps?limit=100');
        let allBuckets = [];

        for (const app of appsData.items) {
            try {
                const bData = await apiRequest(`/apps/${app.app_id}/buckets?limit=100`);
                allBuckets = allBuckets.concat(bData.items.map(b => ({ ...b, app_name: app.name })));
            } catch (_) { }
        }

        buckets = allBuckets;
        const select = document.getElementById('file-bucket-select');
        select.innerHTML = '<option value="">选择存储桶</option>' +
            allBuckets.map(b => `<option value="${escapeHtml(b.name)}">${escapeHtml(b.name)} (${escapeHtml(b.app_name)})</option>`).join('');

        if (currentBucket && allBuckets.some(b => b.name === currentBucket)) {
            select.value = currentBucket;
            loadFiles();
        }
    } catch (e) {
        showToast(e.message, 'error');
    }
}

document.getElementById('file-bucket-select').addEventListener('change', (e) => {
    currentBucket = e.target.value;
    fileMarker = '';
    if (currentBucket) {
        loadFiles();
    } else {
        document.getElementById('files-tbody').innerHTML = '';
    }
});

document.getElementById('file-prefix-input').addEventListener('change', (e) => {
    filePrefix = e.target.value;
    fileMarker = '';
    loadFiles();
});

async function loadFiles() {
    if (!currentBucket) return;

    try {
        // Use admin endpoint for listing files
        const data = await apiRequest(`/buckets/${currentBucket}/objects?prefix=${encodeURIComponent(filePrefix)}&marker=${encodeURIComponent(fileMarker)}&max-keys=50`);
        renderFiles(data);
    } catch (e) {
        showToast(e.message, 'error');
    }
}

function renderFiles(data) {
    const tbody = document.getElementById('files-tbody');

    if (data.contents.length === 0) {
        tbody.innerHTML = '<tr><td colspan="8" style="text-align:center;color:var(--text-secondary);padding:40px">暂无文件</td></tr>';
    } else {
        tbody.innerHTML = data.contents.map(f => `
            <tr>
                <td><input type="checkbox" class="file-check" value="${escapeHtml(f.key)}"></td>
                <td>${escapeHtml(f.key)}</td>
                <td>${formatSize(f.size)}</td>
                <td><code style="font-size:12px">${escapeHtml(f.content_type)}</code></td>
                <td><code style="font-size:11px">${escapeHtml(f.etag.substring(0, 12))}...</code></td>
                <td>${formatDate(f.last_modified)}</td>
                <td class="action-btns">
                    <button class="btn btn-sm btn-secondary" onclick="previewFile('${escapeHtml(f.key)}')">预览</button>
                    <button class="btn btn-sm btn-secondary" onclick="copyFile('${escapeHtml(f.key)}')">复制</button>
                    <button class="btn btn-sm btn-secondary" onclick="downloadFile('${escapeHtml(f.key)}')">下载</button>
                    <button class="btn btn-sm btn-danger" onclick="deleteFile('${escapeHtml(f.key)}')">删除</button>
                </td>
            </tr>
        `).join('');
    }

    const checkAll = document.getElementById('files-check-all');
    if (checkAll) checkAll.checked = false;

    // Simple pagination with marker
    const pagEl = document.getElementById('files-pagination');
    if (data.is_truncated && data.contents.length > 0) {
        const lastKey = data.contents[data.contents.length - 1].key;
        pagEl.innerHTML = `
            <button onclick="loadMoreFiles('${escapeHtml(lastKey)}')">加载更多</button>
        `;
    } else {
        pagEl.innerHTML = '';
    }
}

async function loadMoreFiles(marker) {
    fileMarker = marker;
    try {
        const data = await apiRequest(`/buckets/${currentBucket}/objects?prefix=${encodeURIComponent(filePrefix)}&marker=${encodeURIComponent(fileMarker)}&max-keys=50`);
        // Append to existing table
        const tbody = document.getElementById('files-tbody');
        const existingRows = tbody.innerHTML;
        const newRows = data.contents.map(f => `
            <tr>
                <td>${escapeHtml(f.key)}</td>
                <td>${formatSize(f.size)}</td>
                <td><code style="font-size:12px">${escapeHtml(f.content_type)}</code></td>
                <td><code style="font-size:11px">${escapeHtml(f.etag.substring(0, 12))}...</code></td>
                <td>${formatDate(f.last_modified)}</td>
                <td class="action-btns">
                    <button class="btn btn-sm btn-secondary" onclick="downloadFile('${escapeHtml(f.key)}')">下载</button>
                    <button class="btn btn-sm btn-danger" onclick="deleteFile('${escapeHtml(f.key)}')">删除</button>
                </td>
            </tr>
        `).join('');

        // Remove "load more" placeholder if exists
        if (existingRows.includes('暂无文件')) {
            tbody.innerHTML = newRows;
        } else {
            tbody.innerHTML = existingRows + newRows;
        }

        fileMarker = data.contents.length > 0 ? data.contents[data.contents.length - 1].key : '';

        const pagEl = document.getElementById('files-pagination');
        if (data.is_truncated && data.contents.length > 0) {
            pagEl.innerHTML = `<button onclick="loadMoreFiles('${escapeHtml(fileMarker)}')">加载更多</button>`;
        } else {
            pagEl.innerHTML = '';
        }
    } catch (e) {
        showToast(e.message, 'error');
    }
}

// ---- 文件上传 ----

document.getElementById('upload-btn').addEventListener('click', () => {
    if (!currentBucket) {
        showToast('请先选择存储桶', 'error');
        return;
    }
    document.getElementById('file-input').click();
});

document.getElementById('file-input').addEventListener('change', (e) => {
    const files = Array.from(e.target.files);
    uploadFiles(files);
    e.target.value = '';
});

// Drag and drop
const uploadArea = document.getElementById('upload-area');

uploadArea.addEventListener('dragover', (e) => {
    e.preventDefault();
    uploadArea.classList.add('drag-over');
});

uploadArea.addEventListener('dragleave', () => {
    uploadArea.classList.remove('drag-over');
});

uploadArea.addEventListener('drop', (e) => {
    e.preventDefault();
    uploadArea.classList.remove('drag-over');
    if (!currentBucket) {
        showToast('请先选择存储桶', 'error');
        return;
    }
    const files = Array.from(e.dataTransfer.files);
    uploadFiles(files);
});

async function uploadFiles(files) {
    if (!currentBucket) {
        showToast('请先选择存储桶', 'error');
        return;
    }

    if (files.length === 0) return;

    // Get app credentials for the bucket
    const bucket = buckets.find(b => b.name === currentBucket);
    if (!bucket) {
        showToast('存储桶信息未找到', 'error');
        return;
    }

    // We need app credentials to upload. Since admin API doesn't give us the secret,
    // we need an admin upload endpoint.
    // For simplicity, let's use the admin upload endpoint.

    showToast(`正在上传 ${files.length} 个文件...`, 'info');

    let successCount = 0;
    for (const file of files) {
        try {
            await uploadSingleFile(file);
            successCount++;
        } catch (e) {
            showToast(`${file.name}: ${e.message}`, 'error');
        }
    }

    if (successCount > 0) {
        showToast(`成功上传 ${successCount} 个文件`, 'success');
        loadFiles();
    }
}

async function uploadSingleFile(file) {
    const key = filePrefix ? `${filePrefix}${filePrefix.endsWith('/') ? '' : '/'}${file.name}` : file.name;

    // Use admin upload endpoint
    const res = await fetch(`${API_BASE}/buckets/${currentBucket}/objects/${encodeURIComponent(key)}`, {
        method: 'PUT',
        headers: {
            'X-Admin-Token': localStorage.getItem('admin_token') || '',
            'Content-Type': file.type || 'application/octet-stream',
        },
        body: file,
    });

    if (!res.ok) {
        let msg = '上传失败';
        try {
            const data = await res.json();
            msg = data.message || msg;
        } catch (_) { }
        throw new Error(msg);
    }
}

async function downloadFile(key) {
    // Use admin download endpoint or generate a signed URL
    // For simplicity, use the admin download endpoint
    const url = `${API_BASE}/buckets/${currentBucket}/objects/${encodeURIComponent(key)}`;
    const token = localStorage.getItem('admin_token') || '';

    const res = await fetch(url, {
        headers: { 'X-Admin-Token': token },
    });

    if (!res.ok) {
        showToast('下载失败', 'error');
        return;
    }

    const blob = await res.blob();
    const a = document.createElement('a');
    a.href = URL.createObjectURL(blob);
    a.download = key.split('/').pop();
    a.click();
    URL.revokeObjectURL(a.href);
}

async function deleteFile(key) {
    if (!confirm(`确定要删除文件 "${key}" 吗？文件将移入回收站，可随时还原。`)) return;

    try {
        await apiRequest(`/buckets/${currentBucket}/objects/${encodeURIComponent(key)}`, {
            method: 'DELETE',
        });
        showToast('文件已移入回收站', 'success');
        loadFiles();
    } catch (e) {
        showToast(e.message, 'error');
    }
}

const TEXT_PREVIEW_TYPES = ['application/json', 'application/xml', 'application/javascript'];

async function previewFile(key) {
    const token = localStorage.getItem('admin_token') || '';
    try {
        const res = await fetch(`${API_BASE}/buckets/${currentBucket}/objects/${encodeURIComponent(key)}`, {
            headers: { 'X-Admin-Token': token },
        });
        if (!res.ok) throw new Error(`HTTP ${res.status}`);

        const type = (res.headers.get('content-type') || '').split(';')[0].trim().toLowerCase();
        if (type.startsWith('image/')) {
            const blob = await res.blob();
            const objUrl = URL.createObjectURL(blob);
            openModal(`预览 - ${key}`, `<div style="text-align:center"><img src="${objUrl}" style="max-width:100%;max-height:65vh" onload="URL.revokeObjectURL(this.src)"></div>`);
        } else if (type.startsWith('text/') || TEXT_PREVIEW_TYPES.includes(type)) {
            const content = await res.text();
            const shown = content.length > 60000 ? `${content.slice(0, 60000)}\n…（内容过长，已截断）` : content;
            openModal(`预览 - ${key}`, `<pre style="max-height:65vh;overflow:auto;white-space:pre-wrap;word-break:break-all">${escapeHtml(shown)}</pre>`);
        } else {
            openModal(`预览 - ${key}`, `<p>该文件类型（<code>${escapeHtml(type || '未知')}</code>）不支持在线预览，请下载后查看。</p>`);
        }
    } catch (e) {
        showToast(`预览失败：${e.message}`, 'error');
    }
}

async function copyFile(key) {
    const target = prompt(`将 "${key}" 复制到（输入新对象名，可含 / 前缀）：`, key);
    if (!target || target === key) return;

    const token = localStorage.getItem('admin_token') || '';
    try {
        const src = await fetch(`${API_BASE}/buckets/${currentBucket}/objects/${encodeURIComponent(key)}`, {
            headers: { 'X-Admin-Token': token },
        });
        if (!src.ok) throw new Error(`读取源文件失败（HTTP ${src.status}）`);
        const blob = await src.blob();

        const put = await fetch(`${API_BASE}/buckets/${currentBucket}/objects/${encodeURIComponent(target)}`, {
            method: 'PUT',
            headers: {
                'X-Admin-Token': token,
                'Content-Type': src.headers.get('content-type') || 'application/octet-stream',
            },
            body: blob,
        });
        if (!put.ok) {
            let msg = `HTTP ${put.status}`;
            try { msg = (await put.json()).message || msg; } catch (_) { }
            throw new Error(msg);
        }

        showToast('复制成功', 'success');
        loadFiles();
    } catch (e) {
        showToast(`复制失败：${e.message}`, 'error');
    }
}

async function batchDeleteFiles() {
    const keys = Array.from(document.querySelectorAll('.file-check:checked')).map(cb => cb.value);
    if (keys.length === 0) {
        showToast('请先勾选要删除的文件', 'error');
        return;
    }
    if (!confirm(`确定要将选中的 ${keys.length} 个文件移入回收站吗？`)) return;

    let done = 0;
    for (const key of keys) {
        try {
            await apiRequest(`/buckets/${currentBucket}/objects/${encodeURIComponent(key)}`, {
                method: 'DELETE',
            });
            done++;
        } catch (e) {
            showToast(`${key}: ${e.message}`, 'error');
        }
    }
    if (done > 0) showToast(`已将 ${done} 个文件移入回收站`, 'success');
    loadFiles();
}

document.getElementById('files-check-all').addEventListener('change', (e) => {
    document.querySelectorAll('.file-check').forEach(cb => { cb.checked = e.target.checked; });
});

document.getElementById('files-batch-delete').addEventListener('click', batchDeleteFiles);

// ---- 使用统计 ----

async function loadStats() {
    try {
        const appsData = await apiRequest('/apps?limit=1000');
        let totalFiles = 0;
        let totalSize = 0;
        let totalBuckets = 0;

        for (const app of appsData.items) {
            try {
                const bData = await apiRequest(`/apps/${app.app_id}/buckets?limit=1000`);
                totalBuckets += bData.total;
            } catch (_) { }
        }

        // Total files/size would need aggregate endpoint.
        // For demo, show what we have.
        document.getElementById('stat-apps').textContent = appsData.total;
        document.getElementById('stat-buckets').textContent = totalBuckets;
        document.getElementById('stat-files').textContent = totalFiles;
        document.getElementById('stat-size').textContent = formatSize(totalSize);
    } catch (e) {
        showToast(e.message, 'error');
    }
}

// ---- 分页辅助 ----

function renderPagination(containerId, data, onChange) {
    const container = document.getElementById(containerId);
    const { total, offset, limit } = data;

    if (total <= limit) {
        container.innerHTML = '';
        return;
    }

    const totalPages = Math.ceil(total / limit);
    const currentPage = Math.floor(offset / limit) + 1;

    let html = `<button ${currentPage === 1 ? 'disabled' : ''} onclick="pageChange('${containerId}', 0, ${limit}, '${containerId}')">上一页</button>`;

    const maxVisible = 5;
    let start = Math.max(1, currentPage - Math.floor(maxVisible / 2));
    let end = Math.min(totalPages, start + maxVisible - 1);
    start = Math.max(1, end - maxVisible + 1);

    for (let i = start; i <= end; i++) {
        const pageOffset = (i - 1) * limit;
        html += `<button class="${i === currentPage ? 'active' : ''}" onclick="pageChange('${containerId}', ${pageOffset}, ${limit}, '${containerId}')">${i}</button>`;
    }

    html += `<button ${currentPage === totalPages ? 'disabled' : ''} onclick="pageChange('${containerId}', ${offset + limit}, ${limit}, '${containerId}')">下一页</button>`;
    html += `<span style="margin-left:12px;color:var(--text-secondary);font-size:13px">共 ${total} 条</span>`;

    container.innerHTML = html;
}

function pageChange(containerId, offset, limit, _) {
    // Map container id to load function
    if (containerId === 'apps-pagination') {
        appsOffset = offset;
        loadApps();
    } else if (containerId === 'buckets-pagination') {
        bucketsOffset = offset;
        loadBuckets();
    } else if (containerId === 'trash-pagination') {
        trashOffset = offset;
        loadTrash();
    }
}

// ---- 回收站 ----

let trashOffset = 0;
const TRASH_LIMIT = 20;
let trashAppFilter = '';
let trashBucketFilter = '';

async function loadTrashFilters() {
    const appSelect = document.getElementById('trash-app-select');
    const bucketSelect = document.getElementById('trash-bucket-select');
    trashOffset = 0;

    try {
        const appsData = await apiRequest('/apps?limit=100');
        let allBuckets = [];
        for (const app of appsData.items) {
            try {
                const bData = await apiRequest(`/apps/${app.app_id}/buckets?limit=100`);
                allBuckets = allBuckets.concat(
                    bData.items.map(b => ({ ...b, app_name: app.name }))
                );
            } catch (_) { }
        }

        appSelect.innerHTML = '<option value="">全部应用</option>' +
            appsData.items.map(a => `<option value="${escapeHtml(a.app_id)}">${escapeHtml(a.name)}</option>`).join('');
        bucketSelect.innerHTML = '<option value="">全部存储桶</option>' +
            allBuckets.map(b => `<option value="${escapeHtml(b.bucket_id)}">${escapeHtml(b.name)} (${escapeHtml(b.app_name)})</option>`).join('');
    } catch (e) {
        showToast(e.message, 'error');
    }

    trashAppFilter = '';
    trashBucketFilter = '';
    await loadTrash();
}

async function loadTrash() {
    const params = new URLSearchParams();
    if (trashAppFilter) params.set('app_id', trashAppFilter);
    if (trashBucketFilter) params.set('bucket_id', trashBucketFilter);
    params.set('offset', trashOffset);
    params.set('limit', TRASH_LIMIT);

    try {
        const data = await apiRequest(`/trash?${params.toString()}`);
        renderTrash(data);
    } catch (e) {
        showToast(e.message, 'error');
    }
}

function renderTrash(data) {
    const tbody = document.getElementById('trash-tbody');
    document.getElementById('trash-summary').textContent =
        `共 ${data.total} 项，占用 ${formatSize(data.total_size)}`;
    document.getElementById('trash-check-all').checked = false;

    if (!data.items.length) {
        tbody.innerHTML = '<tr><td colspan="7" style="text-align:center;color:var(--text-secondary);padding:40px">回收站为空</td></tr>';
    } else {
        tbody.innerHTML = data.items.map(item => `
            <tr>
                <td class="col-check"><input type="checkbox" class="trash-check" value="${escapeHtml(item.object_id)}"></td>
                <td>${escapeHtml(item.key)}</td>
                <td>${escapeHtml(item.app_name || item.app_id)}</td>
                <td>${escapeHtml(item.bucket_name || item.bucket_id)}</td>
                <td>${formatSize(item.size)}</td>
                <td>${formatDate(item.deleted_at)}</td>
                <td class="action-btns">
                    <button class="btn btn-sm btn-primary" onclick="restoreTrash('${escapeHtml(item.object_id)}', '${escapeHtml(item.key)}')">还原</button>
                    <button class="btn btn-sm btn-danger" onclick="purgeTrash('${escapeHtml(item.object_id)}', '${escapeHtml(item.key)}')">彻底删除</button>
                </td>
            </tr>
        `).join('');
    }

    renderPagination('trash-pagination', data, null);
}

function selectedTrashIds() {
    return Array.from(document.querySelectorAll('.trash-check:checked')).map(el => el.value);
}

async function restoreTrash(objectId, key) {
    if (!confirm(`确定要把 "${key}" 还原到原存储桶吗？`)) return;
    await doRestore(objectId, false, key);
}

async function doRestore(objectId, overwrite, key) {
    try {
        await apiRequest(`/trash/${objectId}/restore`, {
            method: 'POST',
            body: JSON.stringify({ overwrite }),
        });
        showToast('已还原', 'success');
        loadTrash();
    } catch (e) {
        if (!overwrite && e.message.includes('already occupied')) {
            if (confirm(`"${key}" 在原存储桶中已被占用，是否覆盖？被覆盖的对象会移入回收站。`)) {
                await doRestore(objectId, true, key);
            }
        } else {
            showToast(e.message, 'error');
        }
    }
}

async function purgeTrash(objectId, key) {
    if (!confirm(`确定要彻底删除 "${key}" 吗？磁盘文件将被删除，此操作不可撤销！`)) return;
    try {
        await apiRequest(`/trash/${objectId}`, { method: 'DELETE' });
        showToast('已彻底删除', 'success');
        loadTrash();
    } catch (e) {
        showToast(e.message, 'error');
    }
}

async function batchTrash(action) {
    const ids = selectedTrashIds();
    if (!ids.length) {
        showToast('请先勾选要操作的对象', 'error');
        return;
    }
    if (action === 'purge' && !confirm(`确定彻底删除选中的 ${ids.length} 个对象吗？磁盘文件将被删除，不可撤销！`)) return;
    if (action === 'restore' && !confirm(`确定还原选中的 ${ids.length} 个对象吗？`)) return;

    try {
        const result = await apiRequest('/trash/batch', {
            method: 'POST',
            body: JSON.stringify({ action, object_ids: ids, overwrite: false }),
        });
        if (result.failed && result.failed.length) {
            showToast(`成功 ${result.succeeded} 个，失败 ${result.failed.length} 个：${result.failed[0].reason}`, 'error');
        } else {
            showToast(`操作成功（${result.succeeded} 个）`, 'success');
        }
        loadTrash();
    } catch (e) {
        showToast(e.message, 'error');
    }
}

document.getElementById('trash-app-select').addEventListener('change', (e) => {
    trashAppFilter = e.target.value;
    trashOffset = 0;
    loadTrash();
});

document.getElementById('trash-bucket-select').addEventListener('change', (e) => {
    trashBucketFilter = e.target.value;
    trashOffset = 0;
    loadTrash();
});

document.getElementById('trash-refresh-btn').addEventListener('click', () => loadTrash());
document.getElementById('trash-restore-batch').addEventListener('click', () => batchTrash('restore'));
document.getElementById('trash-purge-batch').addEventListener('click', () => batchTrash('purge'));
document.getElementById('trash-check-all').addEventListener('change', (e) => {
    document.querySelectorAll('.trash-check').forEach(el => { el.checked = e.target.checked; });
});

// ---- 清理与自检 ----

async function loadMaintenance() {
    try {
        const data = await apiRequest('/maintenance/status');
        renderMaintenanceConfig(data.config);
        renderMaintenanceSummary(data.cleanup, data.startup_check);
        renderMaintenanceRun('maintenance-cleanup-info', data.cleanup);
        renderMaintenanceRun('maintenance-check-info', data.startup_check);
    } catch (e) {
        showToast(e.message, 'error');
    }
}

function renderMaintenanceConfig(config) {
    const el = document.getElementById('maintenance-config');
    const items = [
        ['回收站保留期', `${config.trash_retention_days} 天`],
        ['自动清理周期', config.cleanup_interval_minutes > 0 ? `${config.cleanup_interval_minutes} 分钟` : '已禁用'],
        ['单次处理上限', `${config.cleanup_max_objects} 个对象`],
        ['单次耗时上限', `${config.cleanup_max_seconds} 秒`],
        ['启动自检模式', config.startup_check_mode === 'fix' ? '自动修复' : '仅报告'],
        ['Nonce 保留期', `${config.nonce_retention_seconds} 秒`],
    ];
    el.innerHTML = items.map(([label, value]) => `
        <div class="kv-item"><span class="kv-label">${escapeHtml(label)}</span><span class="kv-value">${escapeHtml(value)}</span></div>
    `).join('');
}

function renderMaintenanceSummary(cleanup, check) {
    document.getElementById('maint-purged').textContent = cleanup ? cleanup.purged_objects : 0;
    document.getElementById('maint-freed').textContent = formatSize(cleanup ? cleanup.freed_bytes : 0);
    document.getElementById('maint-invalid').textContent = check ? check.invalid_records : 0;
    document.getElementById('maint-orphan').textContent = check ? check.orphan_files : 0;
}

function renderMaintenanceRun(containerId, run) {
    const el = document.getElementById(containerId);
    if (!run) {
        el.innerHTML = '<span class="muted">暂无运行记录</span>';
        return;
    }
    const statusClass = run.status === 'success' ? 'status-success'
        : run.status === 'partial' ? 'status-partial' : 'status-failed';
    let detailText = run.detail || '';
    try {
        detailText = JSON.stringify(JSON.parse(detailText), null, 2);
    } catch (_) { }
    const detail = detailText ? `<div class="detail-pre">${escapeHtml(detailText)}</div>` : '';
    el.innerHTML = `
        <div class="kv-item"><span class="kv-label">状态</span><span class="kv-value"><span class="status-badge ${statusClass}">${escapeHtml(run.status)}</span></span></div>
        <div class="kv-item"><span class="kv-label">触发方式</span><span class="kv-value">${escapeHtml(run.trigger)}</span></div>
        <div class="kv-item"><span class="kv-label">开始时间</span><span class="kv-value">${formatDate(run.started_at)}</span></div>
        <div class="kv-item"><span class="kv-label">耗时</span><span class="kv-value">${run.duration_ms} ms</span></div>
        <div class="kv-item"><span class="kv-label">处理条数</span><span class="kv-value">${run.scanned}</span></div>
        <div class="kv-item"><span class="kv-label">删除对象</span><span class="kv-value">${run.purged_objects}</span></div>
        <div class="kv-item"><span class="kv-label">释放空间</span><span class="kv-value">${formatSize(run.freed_bytes)}</span></div>
        <div class="kv-item"><span class="kv-label">失效记录 / 孤儿文件</span><span class="kv-value">${run.invalid_records} / ${run.orphan_files}（${formatSize(run.orphan_bytes)}）</span></div>
        <div class="kv-item"><span class="kv-label">清理 nonce</span><span class="kv-value">${run.purged_nonces}</span></div>
        <div class="kv-item"><span class="kv-label">修复数量</span><span class="kv-value">${run.fixed}</span></div>
        <div class="kv-item"><span class="kv-label">失败原因</span><span class="kv-value">${escapeHtml(run.error || '无')}</span></div>
        ${detail}
    `;
}

async function triggerMaintenance(kind, mode) {
    const button = document.getElementById(
        kind === 'cleanup' ? 'maintenance-cleanup-btn'
            : mode === 'fix' ? 'maintenance-fix-btn' : 'maintenance-check-btn'
    );
    button.disabled = true;
    try {
        const data = kind === 'cleanup'
            ? await apiRequest('/maintenance/cleanup', { method: 'POST' })
            : await apiRequest('/maintenance/startup-check', {
                method: 'POST',
                body: JSON.stringify({ mode }),
            });
        if (kind === 'cleanup') {
            showToast(`清理完成：删除 ${data.purged_objects} 个对象，释放 ${formatSize(data.freed_bytes)}`, 'success');
        } else {
            showToast(`自检完成：失效记录 ${data.invalid_records} 个，孤儿文件 ${data.orphan_files} 个`, 'success');
        }
        await loadMaintenance();
    } catch (e) {
        showToast(e.message, 'error');
    } finally {
        button.disabled = false;
    }
}

document.getElementById('maintenance-cleanup-btn').addEventListener('click', () => triggerMaintenance('cleanup'));
document.getElementById('maintenance-check-btn').addEventListener('click', () => triggerMaintenance('check', 'report'));
document.getElementById('maintenance-fix-btn').addEventListener('click', () => {
    if (confirm('自检并修复会删除失效记录与孤儿文件，确定继续吗？')) {
        triggerMaintenance('check', 'fix');
    }
});
document.getElementById('maintenance-refresh-btn').addEventListener('click', () => loadMaintenance());

// ---- 工具：HTML 转义 ----

function escapeHtml(str) {
    const div = document.createElement('div');
    div.textContent = str;
    return div.innerHTML;
}

// ---- 初始化 ----

document.getElementById('login-form').addEventListener('submit', async (e) => {
    e.preventDefault();
    const token = document.getElementById('admin-token').value;
    if (!token) return;

    try {
        // Test login by fetching apps
        await apiRequest('/apps?limit=1', {
            headers: { 'X-Admin-Token': token },
        });
        login(token);
    } catch (err) {
        document.getElementById('login-error').textContent = 'Token 无效，请重试';
    }
});

document.getElementById('logout-btn').addEventListener('click', logout);

// Close modal on backdrop click
document.getElementById('modal').addEventListener('click', (e) => {
    if (e.target.id === 'modal') closeModal();
});

// Route handling
window.addEventListener('hashchange', handleRoute);

// Init
if (checkAuth()) {
    handleRoute();
}
