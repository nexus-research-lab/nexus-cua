package nexuscua

import (
	"math"
	"testing"
)

func TestMarshalPressPointUsesClosedWireVariant(t *testing.T) {
	payload, err := marshalAction(PressPoint{
		Point: ScreenshotPoint{X: 12, Y: 34}, Button: PointerRight, DurationMS: 750,
	})
	if err != nil {
		t.Fatal(err)
	}
	if payload["kind"] != "press_point" || payload["duration_ms"] != float64(750) {
		t.Fatalf("unexpected press_point payload: %#v", payload)
	}
}

func TestMarshalBackgroundClickUsesClosedDelivery(t *testing.T) {
	payload, err := marshalAction(ClickPoint{
		Point:    ScreenshotPoint{X: 12, Y: 34},
		Button:   PointerLeft,
		Count:    1,
		Delivery: DeliveryPreferenceBackground,
	})
	if err != nil {
		t.Fatal(err)
	}
	if payload["kind"] != "click_point" || payload["delivery"] != "background" {
		t.Fatalf("unexpected background click payload: %#v", payload)
	}
}

func TestMarshalWindowLifecycleUsesClosedWireVariants(t *testing.T) {
	bounds, err := marshalAction(SetWindowBounds{
		Bounds: ScreenRect{X: -100, Y: 40, Width: 900, Height: 700},
	})
	if err != nil {
		t.Fatal(err)
	}
	if bounds["kind"] != "set_window_bounds" {
		t.Fatalf("unexpected set_window_bounds payload: %#v", bounds)
	}
	minimized, err := marshalAction(SetWindowMinimized{Minimized: false})
	if err != nil {
		t.Fatal(err)
	}
	if minimized["kind"] != "set_window_minimized" || minimized["minimized"] != false {
		t.Fatalf("unexpected set_window_minimized payload: %#v", minimized)
	}
	terminated, err := marshalAction(TerminateApplication{})
	if err != nil {
		t.Fatal(err)
	}
	if terminated["kind"] != "terminate_application" || len(terminated) != 1 {
		t.Fatalf("unexpected terminate_application payload: %#v", terminated)
	}
}

func TestMarshalWindowBoundsRejectsInvalidGeometry(t *testing.T) {
	_, err := marshalAction(SetWindowBounds{
		Bounds: ScreenRect{X: 0, Y: 0, Width: 0, Height: 700},
	})
	if err == nil {
		t.Fatal("expected invalid window bounds to be rejected")
	}
}

func TestMarshalElementStateUsesClosedWireVariant(t *testing.T) {
	name := "Remember me"
	payload, err := marshalPredicate(ElementStateMatches{
		Name: &name, State: ElementStateChecked, Value: true,
	})
	if err != nil {
		t.Fatal(err)
	}
	if payload["kind"] != "element_state" || payload["state"] != "checked" || payload["value"] != true {
		t.Fatalf("unexpected element_state payload: %#v", payload)
	}
}

func TestMarshalElementStateRejectsOpenValue(t *testing.T) {
	_, err := marshalPredicate(ElementStateMatches{State: ElementState("mixed"), Value: true})
	if err == nil {
		t.Fatal("expected invalid element state to be rejected")
	}
}

func TestMarshalSetRangeValueUsesClosedWireVariant(t *testing.T) {
	payload, err := marshalAction(SetRangeValue{ElementRef: "element-1", Value: 42.5})
	if err != nil {
		t.Fatal(err)
	}
	if payload["kind"] != "set_range_value" || payload["value"] != 42.5 {
		t.Fatalf("unexpected set_range_value payload: %#v", payload)
	}
}

func TestMarshalSetRangeValueRejectsNonFiniteValue(t *testing.T) {
	_, err := marshalAction(SetRangeValue{ElementRef: "element-1", Value: math.Inf(1)})
	if err == nil {
		t.Fatal("expected non-finite numeric range value to be rejected")
	}
}
