# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for wlan_using_sl4f.py."""

import unittest
from unittest import mock

from honeydew import errors
from honeydew.affordances.connectivity.wlan.utils import errors as wlan_errors
from honeydew.affordances.connectivity.wlan.utils.types import (
    BssDescription,
    BssType,
    ChannelBandwidth,
    ClientStatusConnected,
    ClientStatusConnecting,
    ClientStatusIdle,
    CountryCode,
    Protection,
    QueryIfaceResponse,
    WlanChannel,
    WlanMacRole,
)
from honeydew.affordances.connectivity.wlan.wlan import wlan_using_sl4f
from honeydew.transports import sl4f as sl4f_transport

_TEST_BSS_DESC_1 = BssDescription(
    bssid=[1, 2, 3],
    bss_type=BssType.PERSONAL,
    beacon_period=2,
    capability_info=3,
    ies=[3, 2, 1],
    channel=WlanChannel(primary=1, cbw=ChannelBandwidth.CBW20, secondary80=3),
    rssi_dbm=4,
    snr_db=5,
)

_TEST_BSS_DESC_2 = BssDescription(
    bssid=[1, 2],
    bss_type=BssType.PERSONAL,
    beacon_period=2,
    capability_info=3,
    ies=[3, 2],
    channel=WlanChannel(primary=1, cbw=ChannelBandwidth.CBW20, secondary80=3),
    rssi_dbm=4,
    snr_db=5,
)


# pylint: disable=protected-access
class WlanSL4FTests(unittest.TestCase):
    """Unit tests for wlan_using_sl4f.py."""

    def setUp(self) -> None:
        super().setUp()

        self.sl4f_obj = mock.MagicMock(spec=sl4f_transport.SL4F)
        self.wlan_obj = wlan_using_sl4f.Wlan(
            device_name="fuchsia-emulator", sl4f=self.sl4f_obj
        )
        self.sl4f_obj.reset_mock()

    def test_connect_success(self) -> None:
        """Test for Wlan.connect()."""
        self.sl4f_obj.run.return_value = {"result": True}
        self.assertEqual(
            self.wlan_obj.connect(
                ssid="test", password="password", bss_desc=_TEST_BSS_DESC_1
            ),
            True,
        )
        self.sl4f_obj.run.assert_called()

    def test_connect_failure_raise_honeydew_wlan_error(self) -> None:
        """Test for Wlan.connect()."""
        self.sl4f_obj.run.side_effect = errors.Sl4fError("fail")
        with self.assertRaises(wlan_errors.HoneydewWlanError):
            self.wlan_obj.connect(
                ssid="test",
                password="password",
                bss_desc=_TEST_BSS_DESC_1,
            )
        self.sl4f_obj.run.assert_called()

    def test_connect_failure_raise_no_wlan_interface_found_error(self) -> None:
        """Test for Wlan.connect()."""
        self.sl4f_obj.run.return_value = {"error": "No wlan interface found"}
        with self.assertRaises(wlan_errors.NetworkInterfaceNotFoundError):
            self.wlan_obj.connect(
                ssid="test",
                password="password",
                bss_desc=_TEST_BSS_DESC_1,
            )
        self.sl4f_obj.run.assert_called()

    def test_connect_failure_raise_interface_role_not_found_error(self) -> None:
        """Test for Wlan.connect()."""
        self.sl4f_obj.run.return_value = {
            "error": "interface with role client not found"
        }
        with self.assertRaises(wlan_errors.NetworkInterfaceNotFoundError):
            self.wlan_obj.connect(
                ssid="test",
                password="password",
                bss_desc=_TEST_BSS_DESC_1,
            )
        self.sl4f_obj.run.assert_called()

    def test_connect_failure_resp_not_bool(self) -> None:
        """Test for Wlan.connect()."""
        self.sl4f_obj.run.return_value = {"result": "not_bool"}
        with self.assertRaises(TypeError):
            self.wlan_obj.connect(
                ssid="test",
                password="password",
                bss_desc=_TEST_BSS_DESC_1,
            )
        self.sl4f_obj.run.assert_called()

    def test_create_iface_success(self) -> None:
        """Test for Wlan.create_iface()."""
        self.sl4f_obj.run.return_value = {"result": 1}
        self.assertEqual(
            self.wlan_obj.create_iface(
                phy_id=1, role=WlanMacRole.CLIENT, sta_addr="test"
            ),
            1,
        )
        self.sl4f_obj.run.assert_called()

    def test_create_iface_failure_raise_honeydew_wlan_error(self) -> None:
        """Test for Wlan.create_iface()."""
        self.sl4f_obj.run.side_effect = errors.Sl4fError("fail")
        with self.assertRaises(wlan_errors.HoneydewWlanError):
            self.wlan_obj.create_iface(
                phy_id=1, role=WlanMacRole.CLIENT, sta_addr="test"
            )
        self.sl4f_obj.run.assert_called()

    def test_create_iface_failure_resp_not_int(self) -> None:
        """Test for Wlan.create_iface()."""
        self.sl4f_obj.run.return_value = {"result": "not_int"}
        with self.assertRaises(TypeError):
            self.wlan_obj.create_iface(
                phy_id=1, role=WlanMacRole.CLIENT, sta_addr="test"
            )
        self.sl4f_obj.run.assert_called()

    def test_destroy_iface(self) -> None:
        """Test for Wlan.destroy_iface()."""
        self.wlan_obj.destroy_iface(iface_id=1)
        self.sl4f_obj.run.assert_called()

    def test_destroy_iface_failure_honeydew_wlan_error(self) -> None:
        """Test for Wlan.destroy_iface()."""
        self.sl4f_obj.run.side_effect = errors.Sl4fError("fail")
        with self.assertRaises(wlan_errors.HoneydewWlanError):
            self.wlan_obj.destroy_iface(iface_id=1)
        self.sl4f_obj.run.assert_called()

    def test_disconnect(self) -> None:
        """Test for Wlan.disconnect()."""
        self.wlan_obj.disconnect()
        self.sl4f_obj.run.assert_called()

    def test_disconnect_failure_honeydew_wlan_error(self) -> None:
        """Test for Wlan.disconnect()."""
        self.sl4f_obj.run.side_effect = errors.Sl4fError("fail")
        with self.assertRaises(wlan_errors.HoneydewWlanError):
            self.wlan_obj.disconnect()
        self.sl4f_obj.run.assert_called()

    def test_get_country_success(self) -> None:
        """Test for Wlan.get_country()."""
        self.sl4f_obj.run.return_value = {"result": [85, 83]}
        self.assertEqual(self.wlan_obj.get_country(1), "US")
        self.sl4f_obj.run.assert_called()

    def test_get_country_failure_honeydew_wlan_error(self) -> None:
        """Test for Wlan.get_country()."""
        self.sl4f_obj.run.side_effect = errors.Sl4fError("fail")
        with self.assertRaises(wlan_errors.HoneydewWlanError):
            self.wlan_obj.get_country(1)
        self.sl4f_obj.run.assert_called()

    def test_get_country_failure_resp_not_str(self) -> None:
        """Test for Wlan.get_country()."""
        self.sl4f_obj.run.return_value = {"result": "a"}
        with self.assertRaises(TypeError):
            self.wlan_obj.get_country(1)
        self.sl4f_obj.run.assert_called()

    def test_get_iface_id_list_success(self) -> None:
        """Test for Wlan.get_iface_id_list()."""
        self.sl4f_obj.run.return_value = {"result": [1, 2, 3]}
        self.assertEqual(self.wlan_obj.get_iface_id_list(), [1, 2, 3])
        self.sl4f_obj.run.assert_called()

    def test_get_iface_id_list_failure_honeydew_wlan_error(self) -> None:
        """Test for Wlan.get_iface_id_list()."""
        self.sl4f_obj.run.side_effect = errors.Sl4fError("fail")
        with self.assertRaises(wlan_errors.HoneydewWlanError):
            self.wlan_obj.get_iface_id_list()
        self.sl4f_obj.run.assert_called()

    def test_get_iface_id_list_failure(self) -> None:
        """Test for Wlan.get_iface_id_list()."""
        self.sl4f_obj.run.return_value = {"result": "not_list"}
        with self.assertRaises(TypeError):
            self.wlan_obj.get_iface_id_list()
        self.sl4f_obj.run.assert_called()

    def test_get_phy_id_list_success(self) -> None:
        """Test for Wlan.get_phy_id_list()."""
        self.sl4f_obj.run.return_value = {"result": [1, 2, 3]}
        self.assertEqual(self.wlan_obj.get_phy_id_list(), [1, 2, 3])
        self.sl4f_obj.run.assert_called()

    def test_get_phy_id_list_failure_honeydew_wlan_error(self) -> None:
        """Test for Wlan.get_phy_id_list()."""
        self.sl4f_obj.run.side_effect = errors.Sl4fError("fail")
        with self.assertRaises(wlan_errors.HoneydewWlanError):
            self.wlan_obj.get_phy_id_list()
        self.sl4f_obj.run.assert_called()

    def test_get_phy_id_list_failure(self) -> None:
        """Test for Wlan.get_phy_id_list()."""
        self.sl4f_obj.run.return_value = {"result": "not_list"}
        with self.assertRaises(TypeError):
            self.wlan_obj.get_phy_id_list()
        self.sl4f_obj.run.assert_called()

    def test_query_iface_success(self) -> None:
        """Test for Wlan.query_iface()."""
        self.sl4f_obj.run.return_value = {
            "result": {
                "role": "Client",
                "id": 1,
                "phy_id": 2,
                "phy_assigned_id": 3,
                "sta_addr": [1, 2],
            }
        }

        expected_value = QueryIfaceResponse(
            role=WlanMacRole.CLIENT,
            id=1,
            phy_id=2,
            phy_assigned_id=3,
            sta_addr=[1, 2],
        )

        self.assertEqual(self.wlan_obj.query_iface(1), expected_value)
        self.sl4f_obj.run.assert_called()

    def test_query_iface_failure_honeydew_wlan_error(self) -> None:
        """Test for Wlan.query_iface()."""
        self.sl4f_obj.run.side_effect = errors.Sl4fError("fail")
        with self.assertRaises(wlan_errors.HoneydewWlanError):
            self.wlan_obj.query_iface(1)
        self.sl4f_obj.run.assert_called()

    def test_query_iface_failure_resp_not_dict(self) -> None:
        """Test for Wlan.query_iface()."""
        self.sl4f_obj.run.return_value = {"result": "not_dict"}
        with self.assertRaises(TypeError):
            self.wlan_obj.query_iface(1)
        self.sl4f_obj.run.assert_called()

    def test_query_iface_failure_resp_sta_addr_not_list(self) -> None:
        """Test for Wlan.query_iface()."""
        self.sl4f_obj.run.return_value = {"result": {"sta_addr": "not_list"}}
        with self.assertRaises(TypeError):
            self.wlan_obj.query_iface(1)
        self.sl4f_obj.run.assert_called()

    def test_scan_for_bss_info_success(self) -> None:
        """Test for Wlan.scan_for_bss_info()."""
        self.sl4f_obj.run.return_value = {
            "result": {
                "bss1": [
                    {
                        "bssid": [1, 2, 3],
                        "bss_type": "Personal",
                        "beacon_period": 2,
                        "capability_info": 3,
                        "ies": [3, 2, 1],
                        "channel": {
                            "primary": 1,
                            "cbw": "Cbw20",
                            "secondary80": 3,
                        },
                        "rssi_dbm": 4,
                        "snr_db": 5,
                    },
                    {
                        "bssid": [1, 2],
                        "bss_type": "Personal",
                        "beacon_period": 2,
                        "capability_info": 3,
                        "ies": [3, 2],
                        "channel": {
                            "primary": 1,
                            "cbw": "Cbw20",
                            "secondary80": 3,
                        },
                        "rssi_dbm": 4,
                        "snr_db": 5,
                    },
                ],
            }
        }

        self.assertEqual(
            self.wlan_obj.scan_for_bss_info(),
            {"bss1": [_TEST_BSS_DESC_1, _TEST_BSS_DESC_2]},
        )

        self.sl4f_obj.run.assert_called()

    def test_scan_for_bss_info_failure_honeydew_wlan_error(self) -> None:
        """Test for Wlan.scan_for_bss_info()."""
        self.sl4f_obj.run.side_effect = errors.Sl4fError("fail")
        with self.assertRaises(wlan_errors.HoneydewWlanError):
            self.wlan_obj.scan_for_bss_info()
        self.sl4f_obj.run.assert_called()

    def test_scan_for_bss_info_failure_raise_no_wlan_interface_found_error(
        self,
    ) -> None:
        """Test for Wlan.scan_for_bss_info()."""
        self.sl4f_obj.run.return_value = {"error": "No wlan interface found"}
        with self.assertRaises(wlan_errors.NetworkInterfaceNotFoundError):
            self.wlan_obj.scan_for_bss_info()
        self.sl4f_obj.run.assert_called()

    def test_scan_for_bss_info_failure_raise_interface_role_not_found_error(
        self,
    ) -> None:
        """Test for Wlan.scan_for_bss_info()."""
        self.sl4f_obj.run.return_value = {
            "error": "interface with role client not found"
        }
        with self.assertRaises(wlan_errors.NetworkInterfaceNotFoundError):
            self.wlan_obj.scan_for_bss_info()
        self.sl4f_obj.run.assert_called()

    def test_scan_for_bss_info_failure_resp_not_dict(self) -> None:
        """Test for Wlan.scan_for_bss_info()."""
        self.sl4f_obj.run.return_value = {"result": "not_dict"}
        with self.assertRaises(TypeError):
            self.wlan_obj.scan_for_bss_info()
        self.sl4f_obj.run.assert_called()

    def test_scan_for_bss_info_failure_bss_desc_not_dict(self) -> None:
        """Test for Wlan.scan_for_bss_info()."""
        self.sl4f_obj.run.return_value = {"result": {"bss1": "not_dict"}}
        with self.assertRaises(TypeError):
            self.wlan_obj.scan_for_bss_info()
        self.sl4f_obj.run.assert_called()

    def test_scan_for_bss_info_failure_bssid_not_list(self) -> None:
        """Test for Wlan.scan_for_bss_info()."""
        self.sl4f_obj.run.return_value = {
            "result": {
                "bss1": [
                    {
                        "bssid": "not_list",
                        "ies": [1, 2],
                        "channel": {"primary": 1, "cbw": 0, "secondary80": 1},
                    },
                ],
            }
        }

        with self.assertRaises(TypeError):
            self.wlan_obj.scan_for_bss_info()
        self.sl4f_obj.run.assert_called()

    def test_scan_for_bss_info_failure_ies_not_list(self) -> None:
        """Test for Wlan.scan_for_bss_info()."""
        self.sl4f_obj.run.return_value = {
            "result": {
                "bss1": [
                    {
                        "bssid": [1, 2],
                        "ies": "not_list",
                        "channel": {"primary": 1, "cbw": 0, "secondary80": 1},
                    },
                ],
            }
        }

        with self.assertRaises(TypeError):
            self.wlan_obj.scan_for_bss_info()
        self.sl4f_obj.run.assert_called()

    def test_scan_for_bss_info_failure_channel_not_dict(self) -> None:
        """Test for Wlan.scan_for_bss_info()."""
        self.sl4f_obj.run.return_value = {
            "result": {
                "bss1": [
                    {
                        "bssid": [1, 2],
                        "ies": [1, 2],
                        "channel": "not_dict",
                    },
                ],
            }
        }
        with self.assertRaises(TypeError):
            self.wlan_obj.scan_for_bss_info()
        self.sl4f_obj.run.assert_called()

    def test_set_region_success(self) -> None:
        """Test for Wlan.set_region()."""
        self.wlan_obj.set_region(CountryCode.UNITED_STATES_OF_AMERICA)
        self.sl4f_obj.run.assert_called()

    def test_set_region_failure_honeydew_wlan_error(self) -> None:
        """Test for Wlan.set_region()."""
        self.sl4f_obj.run.side_effect = errors.Sl4fError("fail")
        with self.assertRaises(wlan_errors.HoneydewWlanError):
            self.wlan_obj.set_region(CountryCode.UNITED_STATES_OF_AMERICA)
        self.sl4f_obj.run.assert_called()

    def test_status_idle_success(self) -> None:
        """Test for Wlan.status()."""
        self.sl4f_obj.run.return_value = {"result": {"Idle": None}}
        expected_value = ClientStatusIdle()
        self.assertEqual(self.wlan_obj.status(), expected_value)
        self.sl4f_obj.run.assert_called()

    def test_status_connecting_success(self) -> None:
        """Test for Wlan.status()."""
        self.sl4f_obj.run.return_value = {"result": {"Connecting": [1, 2, 3]}}
        expected_value = ClientStatusConnecting(ssid=[1, 2, 3])
        self.assertEqual(self.wlan_obj.status(), expected_value)
        self.sl4f_obj.run.assert_called()

    def test_status_connected_success(self) -> None:
        """Test for Wlan.status()."""
        self.sl4f_obj.run.return_value = {
            "result": {
                "Connected": {
                    "bssid": [1, 2],
                    "ssid": [1, 2],
                    "rssi_dbm": 4,
                    "snr_db": 5,
                    "channel": {
                        "primary": 1,
                        "cbw": "Cbw20",
                        "secondary80": 3,
                    },
                    "protection": "Open",
                },
            }
        }
        expected_value = ClientStatusConnected(
            bssid=[1, 2],
            ssid=[1, 2],
            rssi_dbm=4,
            snr_db=5,
            channel=WlanChannel(
                primary=1,
                cbw=ChannelBandwidth.CBW20,
                secondary80=3,
            ),
            protection=Protection.OPEN,
        )

        self.assertEqual(self.wlan_obj.status(), expected_value)
        self.sl4f_obj.run.assert_called()

    def test_status_failure_honeydew_wlan_error(self) -> None:
        """Test for Wlan.status()."""
        self.sl4f_obj.run.side_effect = errors.Sl4fError("fail")
        with self.assertRaises(wlan_errors.HoneydewWlanError):
            self.wlan_obj.status()
        self.sl4f_obj.run.assert_called()

    def test_status_failure_raise_no_wlan_interface_found_error(self) -> None:
        """Test for Wlan.status()."""
        self.sl4f_obj.run.return_value = {"error": "No wlan interface found"}
        with self.assertRaises(wlan_errors.NetworkInterfaceNotFoundError):
            self.wlan_obj.status()
        self.sl4f_obj.run.assert_called()

    def test_status_failure_raise_interface_role_not_found_error(self) -> None:
        """Test for Wlan.status()."""
        self.sl4f_obj.run.return_value = {
            "error": "interface with role client not found"
        }
        with self.assertRaises(wlan_errors.NetworkInterfaceNotFoundError):
            self.wlan_obj.status()
        self.sl4f_obj.run.assert_called()

    def test_status_failure_resp_not_dict(self) -> None:
        """Test for Wlan.status()."""
        self.sl4f_obj.run.return_value = {"result": "not_dict"}
        with self.assertRaises(TypeError):
            self.wlan_obj.status()
        self.sl4f_obj.run.assert_called()

    def test_status_failure_connecting_not_list(self) -> None:
        """Test for Wlan.status()."""
        self.sl4f_obj.run.return_value = {"result": {"Connecting": "not_list"}}
        with self.assertRaises(TypeError):
            self.wlan_obj.status()
        self.sl4f_obj.run.assert_called()

    def test_status_failure_connected_not_dict(self) -> None:
        """Test for Wlan.status()."""
        self.sl4f_obj.run.return_value = {"result": {"Connected": "not_dict"}}
        with self.assertRaises(TypeError):
            self.wlan_obj.status()
        self.sl4f_obj.run.assert_called()

    def test_status_failure_connected_channel_not_dict(self) -> None:
        """Test for Wlan.status()."""
        self.sl4f_obj.run.return_value = {
            "result": {
                "Connected": {
                    "bssid": [1, 2],
                    "ssid": [2, 3],
                    "channel": "not_dict",
                }
            }
        }

        with self.assertRaises(TypeError):
            self.wlan_obj.status()
        self.sl4f_obj.run.assert_called()

    def test_status_failure_connected_bssid_not_list(self) -> None:
        """Test for Wlan.status()."""
        self.sl4f_obj.run.return_value = {
            "result": {
                "Connected": {
                    "bssid": "not_list",
                    "ssid": [2, 3],
                    "channel": {
                        "primary": 1,
                        "cbw": "Cbw20",
                        "secondary80": 3,
                    },
                }
            }
        }

        with self.assertRaises(TypeError):
            self.wlan_obj.status()
        self.sl4f_obj.run.assert_called()

    def test_status_failure_connected_ssid_not_list(self) -> None:
        """Test for Wlan.status()."""
        self.sl4f_obj.run.return_value = {
            "result": {
                "Connected": {
                    "bssid": [1, 2],
                    "ssid": "not_list",
                    "channel": {
                        "primary": 1,
                        "cbw": "Cbw20",
                        "secondary80": 3,
                    },
                }
            }
        }

        with self.assertRaises(TypeError):
            self.wlan_obj.status()
        self.sl4f_obj.run.assert_called()

    def test_status_failure_connected_protection_not_str(self) -> None:
        """Test for Wlan.status()."""
        self.sl4f_obj.run.return_value = {
            "result": {
                "Connected": {
                    "bssid": [1, 2],
                    "ssid": [1, 2],
                    "channel": {
                        "primary": 1,
                        "cbw": "Cbw20",
                        "secondary80": 3,
                    },
                    "protection": 1,
                }
            }
        }

        with self.assertRaises(TypeError):
            self.wlan_obj.status()
        self.sl4f_obj.run.assert_called()


if __name__ == "__main__":
    unittest.main()
