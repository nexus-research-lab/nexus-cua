import unittest

import nexus_cua
from nexus_cua.types import action_wire, predicate_wire


class ActionTests(unittest.TestCase):
    def test_normalized_fraction_and_invalid_values(self) -> None:
        self.assertEqual(nexus_cua.NormalizedPoint(0.25, 999.75).to_wire(), {"x": 0.25, "y": 999.75})
        for value in [float("nan"), float("inf"), -0.1, 1000.1, True]:
            with self.assertRaises(ValueError):
                nexus_cua.NormalizedPoint(value, 0).to_wire()

    def test_press_point_wire_variant(self) -> None:
        payload = action_wire(
            nexus_cua.PressPoint(
                point=nexus_cua.NormalizedPoint(x=12, y=34),
                duration_ms=750,
                button=nexus_cua.PointerButton.RIGHT,
            )
        )
        self.assertEqual(
            payload,
            {
                "kind": "press_point",
                "point": {"x": 12, "y": 34},
                "button": "right",
                "duration_ms": 750,
            },
        )

    def test_background_click_wire_variant(self) -> None:
        self.assertEqual(
            action_wire(
                nexus_cua.ClickPoint(
                    point=nexus_cua.NormalizedPoint(x=12, y=34),
                    delivery=nexus_cua.DeliveryPreference.BACKGROUND,
                )
            ),
            {
                "kind": "click_point",
                "point": {"x": 12, "y": 34},
                "button": "left",
                "count": 1,
                "delivery": "background",
            },
        )

    def test_element_state_wire_variant(self) -> None:
        payload = predicate_wire(
            nexus_cua.ElementStateMatches(
                state=nexus_cua.ElementState.CHECKED,
                value=True,
                name="Remember me",
            )
        )
        self.assertEqual(
            payload,
            {
                "kind": "element_state",
                "role": None,
                "name": "Remember me",
                "state": "checked",
                "value": True,
            },
        )

    def test_window_lifecycle_wire_variants(self) -> None:
        self.assertEqual(
            action_wire(
                nexus_cua.SetWindowBounds(
                    nexus_cua.ScreenRect(x=-100, y=40, width=900, height=700)
                )
            ),
            {
                "kind": "set_window_bounds",
                "bounds": {"x": -100, "y": 40, "width": 900, "height": 700},
            },
        )
        self.assertEqual(
            action_wire(nexus_cua.SetWindowMinimized(minimized=False)),
            {"kind": "set_window_minimized", "minimized": False},
        )
        self.assertEqual(
            action_wire(nexus_cua.TerminateApplication()),
            {"kind": "terminate_application"},
        )

    def test_window_bounds_reject_invalid_geometry(self) -> None:
        with self.assertRaises(ValueError):
            action_wire(
                nexus_cua.SetWindowBounds(
                    nexus_cua.ScreenRect(x=0, y=0, width=0, height=700)
                )
            )

    def test_set_range_value_wire_variant(self) -> None:
        self.assertEqual(
            action_wire(
                nexus_cua.SetRangeValue(
                    element_ref=nexus_cua.ElementRef("element-1"), value=42.5
                )
            ),
            {
                "kind": "set_range_value",
                "element_ref": "element-1",
                "value": 42.5,
            },
        )

    def test_set_range_value_rejects_non_finite_value(self) -> None:
        with self.assertRaises(ValueError):
            action_wire(
                nexus_cua.SetRangeValue(
                    element_ref=nexus_cua.ElementRef("element-1"), value=float("inf")
                )
            )


if __name__ == "__main__":
    unittest.main()
