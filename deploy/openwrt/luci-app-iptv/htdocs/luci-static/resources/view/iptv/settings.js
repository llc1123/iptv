'use strict';
'require form';
'require ui';
'require view';

return view.extend({
	render: function() {
		var m, s, o;

		m = new form.Map('iptv', _('IPTV 媒体服务器'),
			_('服务在 IPTV 接口上获取租约，并使该网络不进入本机路由表。'));

		s = m.section(form.NamedSection, 'main', 'server', _('常规设置'));
		s.anonymous = true;
		s.addremove = false;

		o = s.option(form.Flag, 'enabled', _('启用'));
		o.default = '1';
		o.rmempty = false;

		o = s.option(form.Flag, 'managed', _('由 netifd 管理'),
			_('改为从使用 iptv 协议的网络接口获取地址，而不是在此处获取租约。该接口必须命名为 iptv，因为服务按名称读取其地址并响应其事件。'));
		o.default = '0';
		o.rmempty = false;

		o = s.option(form.Flag, 'bypass_proxy', _('绕过透明代理'),
			_('让 IPTV 服务直连运营商网络，绕过 OpenClash 等兼容的本机透明代理。'));
		o.default = '1';
		o.rmempty = false;

		o = s.option(form.Value, 'interface', _('IPTV 接口'),
			_('连接运营商的接口，例如有专用 IPTV 网口的路由器上的 eth1。'));
		o.default = 'eth1';
		o.rmempty = false;
		o.placeholder = 'eth1';

		o = s.option(form.Value, 'host', _('监听地址'));
		o.default = '0.0.0.0';
		o.rmempty = false;

		o = s.option(form.Value, 'port', _('监听端口'));
		o.datatype = 'port';
		o.default = '8080';
		o.rmempty = false;

		o = s.option(form.Value, 'route_table', _('路由表'),
			_('存放 IPTV 子网和默认路由的路由表，由相应的策略规则选用。必须小于 main 表（32766）。由 netifd 管理接口时忽略此项，改由 IPTV 接口上的 `option ip4table` 决定。'));
		o.datatype = 'range(1,32765)';
		o.validate = function(section_id, value) {
			var n = Number(value);

			// The server's parser rejects these too; failing here keeps procd
			// from respawning a server that cannot start.
			if (n >= 253 && n <= 255)
				return _('路由表 253-255 由内核保留。');

			return true;
		};
		o.default = '4788';
		o.rmempty = false;

		s = m.section(form.NamedSection, 'main', 'server', _('账号认证'));
		s.anonymous = true;
		s.addremove = false;

		o = s.option(form.Value, 'dhcp_user', _('DHCP 用户名'));
		o.rmempty = false;
		// netifd's iptv interface holds these in managed mode.
		o.depends('managed', '0');

		o = s.option(form.Value, 'dhcp_password', _('DHCP 密码'));
		o.password = true;
		o.rmempty = false;
		// netifd's iptv interface holds these in managed mode.
		o.depends('managed', '0');

		o = s.option(form.Value, 'epg_user', _('EPG 用户名'));
		o.rmempty = false;

		o = s.option(form.Value, 'epg_password', _('EPG 密码'));
		o.password = true;
		o.rmempty = false;

		o = s.option(form.Value, 'epg_mac', _('STB MAC 地址'),
			_('可选。默认使用 IPTV 接口的 MAC 地址。'));
		o.datatype = 'macaddr';
		o.placeholder = '00:00:00:00:00:00';

		o = s.option(form.Value, 'stb_id_prefix', _('STB ID 前缀'),
			_('可选。添加在由接口 MAC 生成的 DHCP 主机名之前。'));

		o = s.option(form.Value, 'authentication_url', _('认证地址'));
		o.placeholder = 'http://主机:端口/EDS/jsp/AuthenticationURL';

		s = m.section(form.NamedSection, 'main', 'server', _('节目单'));
		s.anonymous = true;
		s.addremove = false;

		o = s.option(form.Value, 'guide_past_days', _('过去天数'));
		o.datatype = 'range(0,30)';
		o.default = '7';

		o = s.option(form.Value, 'guide_future_days', _('未来天数'));
		o.datatype = 'range(0,30)';
		o.default = '2';

		o = s.option(form.Value, 'guide_timezone', _('节目单时区'));
		o.default = 'Asia/Shanghai';
		o.rmempty = false;

		o = s.option(form.Value, 'refresh_interval_seconds', _('刷新间隔'),
			_('两次刷新节目单之间的秒数。'));
		o.datatype = 'uinteger';
		o.default = '3600';
		o.rmempty = false;

		o = s.option(form.Value, 'guide_cache', _('节目单缓存'),
			_('服务启动时从此文件加载节目单。请放在 tmpfs 上，避免每小时向闪存写入节目单。'));
		o.default = '/var/lib/iptv/guide.json';
		o.rmempty = false;

		o = s.option(form.Value, 'logo_dir', _('台标目录'),
			_('存放台标的目录，每个频道一个 PNG 文件，以频道 ID 命名。'));
		o.default = '/etc/iptv/logos';
		o.rmempty = false;

		return m.render();
	}
});
