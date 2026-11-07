## Default Permission

默认权限：检查、增量安装、全量下载、诊断、取消、退出安全模式与跨重启安装。
命令名与 `src/lib.rs` 中的 `generate_handler!` 列表保持一致。

#### This default permission set includes the following:

- `allow-check`
- `allow-install`
- `allow-prepare`
- `allow-launch`
- `allow-download-full`
- `allow-install-full`
- `allow-diagnose`
- `allow-cancel`
- `allow-reset-safety`
- `allow-pending`
- `allow-install-pending`

## Permission Table

<table>
<tr>
<th>Identifier</th>
<th>Description</th>
</tr>


<tr>
<td>

`delta-updater:allow-cancel`

</td>
<td>

Enables the cancel command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`delta-updater:deny-cancel`

</td>
<td>

Denies the cancel command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`delta-updater:allow-check`

</td>
<td>

Enables the check command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`delta-updater:deny-check`

</td>
<td>

Denies the check command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`delta-updater:allow-diagnose`

</td>
<td>

Enables the diagnose command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`delta-updater:deny-diagnose`

</td>
<td>

Denies the diagnose command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`delta-updater:allow-download-full`

</td>
<td>

Enables the download_full command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`delta-updater:deny-download-full`

</td>
<td>

Denies the download_full command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`delta-updater:allow-install`

</td>
<td>

Enables the install command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`delta-updater:deny-install`

</td>
<td>

Denies the install command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`delta-updater:allow-install-full`

</td>
<td>

Enables the install_full command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`delta-updater:deny-install-full`

</td>
<td>

Denies the install_full command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`delta-updater:allow-install-pending`

</td>
<td>

Enables the install_pending command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`delta-updater:deny-install-pending`

</td>
<td>

Denies the install_pending command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`delta-updater:allow-launch`

</td>
<td>

Enables the launch command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`delta-updater:deny-launch`

</td>
<td>

Denies the launch command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`delta-updater:allow-pending`

</td>
<td>

Enables the pending command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`delta-updater:deny-pending`

</td>
<td>

Denies the pending command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`delta-updater:allow-prepare`

</td>
<td>

Enables the prepare command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`delta-updater:deny-prepare`

</td>
<td>

Denies the prepare command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`delta-updater:allow-reset-safety`

</td>
<td>

Enables the reset_safety command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`delta-updater:deny-reset-safety`

</td>
<td>

Denies the reset_safety command without any pre-configured scope.

</td>
</tr>
</table>
