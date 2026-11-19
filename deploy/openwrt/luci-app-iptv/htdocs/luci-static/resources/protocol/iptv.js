'use strict';
'require form';
'require network';

return network.registerProtocol('iptv', {
	getI18n: function() {
		return _('IPTV（DHCP 认证）');
	},

	getIfname: function() {
		return this._ubus('l3_device') || this.get('device');
	},

	getPackageName: function() {
		return 'luci-app-iptv';
	},

	isFloating: function() {
		return false;
	},

	isVirtual: function() {
		return false;
	},

	getDevices: function() {
		return null;
	},

	containsDevice: function(ifname) {
		return (network.getIfnameOf(this.getDevice()) == ifname);
	},

	renderFormOptions: function(s) {
		var o;

		o = s.taboption('general', form.Value, 'dhcp_user', _('DHCP 用户名'));
		o.rmempty = false;

		o = s.taboption('general', form.Value, 'dhcp_password', _('DHCP 密码'));
		o.password = true;
		o.rmempty = false;

		o = s.taboption('general', form.Value, 'stb_id_prefix', _('STB ID 前缀'),
			_('可选。添加在由接口 MAC 生成的 DHCP 主机名之前。'));

	}
});
