using System.Windows;
using System.Windows.Automation;
using System.Windows.Automation.Peers;
using System.Windows.Automation.Provider;
using System.Windows.Controls;
using System.Windows.Input;
using System.Windows.Interop;
using System.Windows.Media;
using System.Windows.Threading;
using System.Runtime.InteropServices;

namespace Nexus.Cua.NativeFixture;

internal static class Program
{
    [STAThread]
    private static void Main()
    {
        var application = new Application
        {
            ShutdownMode = ShutdownMode.OnLastWindowClose,
        };

        void ShowGeneration(FixtureWindow? previous, int generation)
        {
            var window = new FixtureWindow(generation, ShowGeneration);
            application.MainWindow = window;
            window.Show();
            previous?.Close();
        }

        ShowGeneration(null, 1);
        application.Run();
    }
}

internal sealed class FixtureWindow : Window
{
    private readonly int generation;
    private readonly Action<FixtureWindow?, int> replaceWindow;
    private readonly TextBox textField;
    private readonly CheckBox featureToggle;
    private readonly ListBox choices;
    private readonly TreeViewItem advancedOptions;
    private readonly TextBlock stateText;
    private readonly Button dragToken;
    private readonly Button dropZone;
    private Window? occluderWindow;
    private HwndSourceHook? sizeHook;

    private int counter;
    private bool dropped;
    private Point dragStart;

    internal FixtureWindow(int generation, Action<FixtureWindow?, int> replaceWindow)
    {
        this.generation = generation;
        this.replaceWindow = replaceWindow;

        var captureProfile = Environment.GetEnvironmentVariable("NEXUS_CUA_FIXTURE_PROFILE");
        var faultHangMilliseconds = int.TryParse(
            Environment.GetEnvironmentVariable("NEXUS_CUA_FIXTURE_FAULT_HANG_MS"),
            out var configuredHangMilliseconds
        )
            ? Math.Clamp(configuredHangMilliseconds, 500, 30_000)
            : 5_000;
        var faultArmMilliseconds = int.TryParse(
            Environment.GetEnvironmentVariable("NEXUS_CUA_FIXTURE_FAULT_ARM_MS"),
            out var configuredArmMilliseconds
        )
            ? Math.Clamp(configuredArmMilliseconds, 500, 10_000)
            : 3_000;
        var captureWidth = captureProfile == "4k" ? 3840 : captureProfile == "1080p" ? 1920 : 960;
        var captureHeight = captureProfile == "4k" ? 2160 : captureProfile == "1080p" ? 1080 : 720;
        Title = $"Nexus CUA Native Fixture · Generation {generation}";
        Width = captureWidth;
        Height = captureHeight;
        MinWidth = 760;
        MinHeight = 600;
        WindowStartupLocation = WindowStartupLocation.CenterScreen;
        Background = Brushes.White;
        if (captureProfile is "1080p" or "4k")
        {
            WindowStyle = WindowStyle.None;
            ResizeMode = ResizeMode.NoResize;
            SourceInitialized += (_, _) =>
            {
                var source = HwndSource.FromHwnd(new WindowInteropHelper(this).Handle);
                sizeHook = (
                    IntPtr windowHandle,
                    int message,
                    IntPtr wordParameter,
                    IntPtr parameter,
                    ref bool handled
                ) =>
                {
                    if (message == 0x0024)
                    {
                        NativeDisplays.PermitWindowSize(parameter, captureWidth, captureHeight);
                        handled = true;
                    }
                    return IntPtr.Zero;
                };
                source?.AddHook(sizeHook);
            };
        }

        var root = new Grid { Margin = new Thickness(20) };
        root.RowDefinitions.Add(new RowDefinition { Height = new GridLength(42) });
        root.RowDefinitions.Add(new RowDefinition { Height = new GridLength(100) });
        root.RowDefinitions.Add(new RowDefinition { Height = new GridLength(86) });
        root.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
        root.RowDefinitions.Add(new RowDefinition { Height = new GridLength(92) });
        root.RowDefinitions.Add(new RowDefinition { Height = new GridLength(82) });

        var heading = new TextBlock
        {
            Text = "Nexus CUA Native Fixture",
            FontSize = 24,
            FontWeight = FontWeights.SemiBold,
            VerticalAlignment = VerticalAlignment.Center,
        };
        AutomationProperties.SetName(heading, "Fixture Heading");
        root.Children.Add(heading);

        var actionRow = new WrapPanel
        {
            VerticalAlignment = VerticalAlignment.Center,
        };
        Grid.SetRow(actionRow, 1);
        var increment = NamedButton("Increment Counter", "Increment Counter");
        var replace = NamedButton("Replace Fixture Window", "Replace Fixture Window");
        var geometry = NamedButton("Change Fixture Geometry", "Change Fixture Geometry");
        var minimize = NamedButton("Minimize Fixture Window", "Minimize Fixture Window");
        var occluder = NamedButton("Toggle Fixture Occluder", "Toggle Fixture Occluder");
        var nextDisplay = NamedButton("Move Fixture To Next Display", "Move Fixture To Next Display");
        var hangBeforeMutation = NamedButton("Hang Before Mutation", "Hang Before Mutation");
        var hangDuringInvoke = new HangingInvokeButton(faultHangMilliseconds)
        {
            Content = "Hang During Invoke",
        };
        AutomationProperties.SetName(hangDuringInvoke, "Hang During Invoke");
        actionRow.Children.Add(increment);
        actionRow.Children.Add(replace);
        actionRow.Children.Add(geometry);
        actionRow.Children.Add(minimize);
        actionRow.Children.Add(occluder);
        actionRow.Children.Add(nextDisplay);
        actionRow.Children.Add(hangBeforeMutation);
        actionRow.Children.Add(hangDuringInvoke);
        root.Children.Add(actionRow);

        var fields = new Grid { Margin = new Thickness(0, 4, 0, 4) };
        fields.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(140) });
        fields.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(280) });
        fields.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(150) });
        fields.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(280) });
        Grid.SetRow(fields, 2);
        fields.Children.Add(FieldLabel("Fixture Text", 0));
        textField = new TextBox
        {
            Text = "ready",
            Height = 30,
            VerticalContentAlignment = VerticalAlignment.Center,
            Margin = new Thickness(0, 3, 20, 0),
        };
        AutomationProperties.SetName(textField, "Fixture Text");
        Grid.SetColumn(textField, 1);
        fields.Children.Add(textField);
        fields.Children.Add(FieldLabel("Fixture Secure Text", 2));
        var secure = new PasswordBox
        {
            Password = "fixture-secret",
            Height = 30,
            VerticalContentAlignment = VerticalAlignment.Center,
            Margin = new Thickness(0, 3, 0, 0),
        };
        AutomationProperties.SetName(secure, "Fixture Secure Text");
        Grid.SetColumn(secure, 3);
        fields.Children.Add(secure);
        root.Children.Add(fields);

        var content = new Grid { Margin = new Thickness(0, 8, 0, 8) };
        content.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(320) });
        content.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(24) });
        content.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        Grid.SetRow(content, 3);

        var semanticControls = new StackPanel();
        featureToggle = new CheckBox
        {
            Content = "Enable Feature",
            Height = 32,
            VerticalContentAlignment = VerticalAlignment.Center,
        };
        AutomationProperties.SetName(featureToggle, "Enable Feature");
        semanticControls.Children.Add(featureToggle);
        var choicesLabel = new TextBlock
        {
            Text = "Fixture Choices",
            Margin = new Thickness(0, 6, 0, 4),
            FontWeight = FontWeights.Medium,
        };
        AutomationProperties.SetName(choicesLabel, "Fixture Choices Label");
        semanticControls.Children.Add(choicesLabel);
        choices = new ListBox
        {
            Height = 92,
            SelectionMode = SelectionMode.Single,
        };
        AutomationProperties.SetName(choices, "Fixture Choices");
        foreach (var name in new[] { "Alpha", "Beta", "Gamma" })
        {
            var item = new ListBoxItem { Content = name };
            AutomationProperties.SetName(item, name);
            choices.Items.Add(item);
        }
        semanticControls.Children.Add(choices);

        advancedOptions = new TreeViewItem
        {
            Header = "Advanced Options",
            Margin = new Thickness(0, 8, 0, 0),
        };
        AutomationProperties.SetName(advancedOptions, "Advanced Options");
        var advancedText = new TextBlock { Text = "Advanced option is visible" };
        AutomationProperties.SetName(advancedText, "Advanced Option Content");
        advancedOptions.Items.Add(advancedText);
        var tree = new TreeView { Height = 78 };
        AutomationProperties.SetName(tree, "Fixture Advanced Options");
        tree.Items.Add(advancedOptions);
        semanticControls.Children.Add(tree);
        content.Children.Add(semanticControls);

        var scrollPanel = new StackPanel();
        for (var marker = 1; marker <= 20; marker++)
        {
            var markerText = new TextBlock
            {
                Text = $"Scroll Marker {marker}",
                Height = 28,
                Padding = new Thickness(6, 4, 6, 4),
                Background = marker % 2 == 0 ? Brushes.WhiteSmoke : Brushes.White,
            };
            AutomationProperties.SetName(markerText, $"Scroll Marker {marker}");
            scrollPanel.Children.Add(markerText);
        }
        var scrollRegion = new ScrollViewer
        {
            Content = scrollPanel,
            VerticalScrollBarVisibility = ScrollBarVisibility.Visible,
            HorizontalScrollBarVisibility = ScrollBarVisibility.Disabled,
            BorderBrush = Brushes.SlateGray,
            BorderThickness = new Thickness(1),
            Height = 250,
        };
        AutomationProperties.SetName(scrollRegion, "Fixture Scroll Region");
        Grid.SetColumn(scrollRegion, 2);
        content.Children.Add(scrollRegion);
        root.Children.Add(content);

        var dragArea = new Grid { Margin = new Thickness(0, 6, 0, 6) };
        dragArea.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(180) });
        dragArea.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(120) });
        dragArea.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(260) });
        Grid.SetRow(dragArea, 4);
        dragToken = NamedSurface("Drag Token", Brushes.SteelBlue, Brushes.White);
        dropZone = NamedSurface("Drop Zone", Brushes.AliceBlue, Brushes.SteelBlue);
        Grid.SetColumn(dropZone, 2);
        dragArea.Children.Add(dragToken);
        dragArea.Children.Add(dropZone);
        root.Children.Add(dragArea);

        stateText = new TextBlock
        {
            TextWrapping = TextWrapping.Wrap,
            Padding = new Thickness(10),
            Background = Brushes.GhostWhite,
            VerticalAlignment = VerticalAlignment.Stretch,
        };
        Grid.SetRow(stateText, 5);
        root.Children.Add(stateText);
        Content = root;

        if (int.TryParse(
                Environment.GetEnvironmentVariable("NEXUS_CUA_FIXTURE_AUTO_EXIT_MS"),
                out var autoExitMilliseconds
            ) && autoExitMilliseconds >= 1_000)
        {
            var autoExit = new DispatcherTimer
            {
                Interval = TimeSpan.FromMilliseconds(Math.Min(autoExitMilliseconds, 600_000)),
            };
            autoExit.Tick += (_, _) =>
            {
                autoExit.Stop();
                Application.Current.Shutdown();
            };
            autoExit.Start();
        }

        increment.Click += (_, _) =>
        {
            counter++;
            UpdateState();
        };
        replace.Click += (_, _) => this.replaceWindow(this, this.generation + 1);
        geometry.Click += (_, _) =>
        {
            Left = Math.Max(0, Left + 24);
            Top = Math.Max(0, Top + 18);
            Width = 900;
            Height = 680;
        };
        minimize.Click += (_, _) => WindowState = WindowState.Minimized;
        occluder.Click += (_, _) => ToggleOccluder();
        nextDisplay.Click += (_, _) => MoveToNextDisplay();
        hangBeforeMutation.Click += async (_, _) =>
        {
            await Task.Delay(faultArmMilliseconds);
            Thread.Sleep(faultHangMilliseconds);
        };
        textField.TextChanged += (_, _) => UpdateState();
        featureToggle.Checked += (_, _) => UpdateState();
        featureToggle.Unchecked += (_, _) => UpdateState();
        choices.SelectionChanged += (_, _) => UpdateState();
        advancedOptions.Expanded += (_, _) => UpdateState();
        advancedOptions.Collapsed += (_, _) => UpdateState();
        dragToken.PreviewMouseLeftButtonDown += BeginDrag;
        dragToken.PreviewMouseLeftButtonUp += EndDrag;
        if (captureProfile is "1080p" or "4k")
        {
            ContentRendered += (_, _) =>
            {
                var handle = new WindowInteropHelper(this).Handle;
                NativeDisplays.SetWindowPos(
                    handle,
                    IntPtr.Zero,
                    0,
                    0,
                    captureWidth,
                    captureHeight,
                    0x0016
                );
            };
        }

        UpdateState();
    }

    private static Button NamedButton(string text, string name)
    {
        var button = new Button
        {
            Content = text,
            Height = 30,
            Padding = new Thickness(10, 3, 10, 3),
            Margin = new Thickness(0, 0, 10, 0),
        };
        AutomationProperties.SetName(button, name);
        return button;
    }

    private static TextBlock FieldLabel(string text, int column)
    {
        var label = new TextBlock
        {
            Text = text,
            VerticalAlignment = VerticalAlignment.Center,
        };
        AutomationProperties.SetName(label, $"{text} Label");
        Grid.SetColumn(label, column);
        return label;
    }

    private static Button NamedSurface(string name, Brush background, Brush foreground)
    {
        var label = new TextBlock
        {
            Text = name,
            Foreground = foreground,
            FontWeight = FontWeights.SemiBold,
            HorizontalAlignment = HorizontalAlignment.Center,
            VerticalAlignment = VerticalAlignment.Center,
        };
        AutomationProperties.SetName(label, $"{name} Label");
        var surface = new Button
        {
            Content = label,
            Width = 150,
            Height = 64,
            Background = background,
            BorderBrush = Brushes.SteelBlue,
            BorderThickness = new Thickness(2),
            HorizontalAlignment = HorizontalAlignment.Left,
            Cursor = Cursors.Hand,
        };
        AutomationProperties.SetName(surface, name);
        return surface;
    }

    private void BeginDrag(object sender, MouseButtonEventArgs eventArgs)
    {
        dragStart = eventArgs.GetPosition(dragToken);
        dragToken.CaptureMouse();
        eventArgs.Handled = true;
    }

    private void EndDrag(object sender, MouseButtonEventArgs eventArgs)
    {
        var point = eventArgs.GetPosition(dropZone);
        var startedInside = dragStart.X >= 0
            && dragStart.Y >= 0
            && dragStart.X <= dragToken.ActualWidth
            && dragStart.Y <= dragToken.ActualHeight;
        if (startedInside
            && point.X >= 0
            && point.Y >= 0
            && point.X <= dropZone.ActualWidth
            && point.Y <= dropZone.ActualHeight)
        {
            dropped = true;
            dropZone.Background = Brushes.LightGreen;
            UpdateState();
        }
        dragToken.ReleaseMouseCapture();
        eventArgs.Handled = true;
    }

    private void UpdateState()
    {
        var selected = choices.SelectedItem is ListBoxItem item
            ? item.Content?.ToString() ?? "none"
            : "none";
        var state = $"Fixture State: generation={generation}; counter={counter}; "
            + $"text={textField.Text}; checked={featureToggle.IsChecked == true}; "
            + $"selection={selected}; expanded={advancedOptions.IsExpanded}; dropped={dropped}";
        state = state.Replace("True", "true").Replace("False", "false");
        stateText.Text = state;
        AutomationProperties.SetName(stateText, state);
    }

    private void ToggleOccluder()
    {
        if (occluderWindow is not null)
        {
            occluderWindow.Close();
            occluderWindow = null;
            return;
        }
        var occluder = new Window
        {
            Title = "Nexus CUA Fixture Occluder",
            Width = ActualWidth,
            Height = ActualHeight,
            Left = Left,
            Top = Top,
            WindowStyle = WindowStyle.None,
            ResizeMode = ResizeMode.NoResize,
            Background = new SolidColorBrush(Color.FromRgb(255, 0, 255)),
            ShowInTaskbar = false,
            Topmost = true,
        };
        occluder.Closed += (_, _) => occluderWindow = null;
        occluderWindow = occluder;
        occluder.Show();
    }

    private void MoveToNextDisplay()
    {
        var monitors = NativeDisplays.Enumerate();
        if (monitors.Count < 2)
        {
            return;
        }
        var handle = new WindowInteropHelper(this).Handle;
        var current = NativeDisplays.MonitorFromWindow(handle, 2);
        var currentIndex = monitors.FindIndex(monitor => monitor.Handle == current);
        var target = monitors[(Math.Max(currentIndex, 0) + 1) % monitors.Count].WorkArea;
        NativeDisplays.SetWindowPos(
            handle,
            IntPtr.Zero,
            target.Left + 48,
            target.Top + 48,
            Math.Max(760, (int)ActualWidth),
            Math.Max(600, (int)ActualHeight),
            0x0010
        );
    }
}

internal sealed class HangingInvokeButton : Button
{
    internal HangingInvokeButton(int hangMilliseconds)
    {
        HangMilliseconds = hangMilliseconds;
    }

    internal int HangMilliseconds { get; }

    protected override AutomationPeer OnCreateAutomationPeer()
    {
        return new HangingInvokeAutomationPeer(this);
    }
}

internal sealed class HangingInvokeAutomationPeer : ButtonAutomationPeer, IInvokeProvider
{
    internal HangingInvokeAutomationPeer(HangingInvokeButton owner)
        : base(owner)
    {
    }

    void IInvokeProvider.Invoke()
    {
        var owner = (HangingInvokeButton)Owner;
        Thread.Sleep(owner.HangMilliseconds);
    }
}

internal static class NativeDisplays
{
    internal readonly record struct Monitor(IntPtr Handle, Rectangle WorkArea);

    [StructLayout(LayoutKind.Sequential)]
    private struct NativePoint
    {
        internal int X;
        internal int Y;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct MinMaxInfo
    {
        internal NativePoint Reserved;
        internal NativePoint MaxSize;
        internal NativePoint MaxPosition;
        internal NativePoint MinTrackSize;
        internal NativePoint MaxTrackSize;
    }

    [StructLayout(LayoutKind.Sequential)]
    internal struct Rectangle
    {
        internal int Left;
        internal int Top;
        internal int Right;
        internal int Bottom;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct MonitorInfo
    {
        internal uint Size;
        internal Rectangle MonitorArea;
        internal Rectangle WorkArea;
        internal uint Flags;
    }

    private delegate bool MonitorCallback(IntPtr monitor, IntPtr hdc, IntPtr rectangle, IntPtr data);

    internal static List<Monitor> Enumerate()
    {
        var output = new List<Monitor>();
        MonitorCallback callback = (monitor, _, _, _) =>
        {
            var info = new MonitorInfo { Size = (uint)Marshal.SizeOf<MonitorInfo>() };
            if (GetMonitorInfo(monitor, ref info))
            {
                output.Add(new Monitor(monitor, info.WorkArea));
            }
            return true;
        };
        EnumDisplayMonitors(IntPtr.Zero, IntPtr.Zero, callback, IntPtr.Zero);
        GC.KeepAlive(callback);
        return output;
    }

    internal static void PermitWindowSize(IntPtr parameter, int width, int height)
    {
        var info = Marshal.PtrToStructure<MinMaxInfo>(parameter);
        info.MaxSize.X = Math.Max(info.MaxSize.X, width);
        info.MaxSize.Y = Math.Max(info.MaxSize.Y, height);
        info.MaxTrackSize.X = Math.Max(info.MaxTrackSize.X, width);
        info.MaxTrackSize.Y = Math.Max(info.MaxTrackSize.Y, height);
        Marshal.StructureToPtr(info, parameter, false);
    }

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool EnumDisplayMonitors(
        IntPtr hdc,
        IntPtr clip,
        MonitorCallback callback,
        IntPtr data
    );

    [DllImport("user32.dll", EntryPoint = "GetMonitorInfoW")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool GetMonitorInfo(IntPtr monitor, ref MonitorInfo info);

    [DllImport("user32.dll")]
    internal static extern IntPtr MonitorFromWindow(IntPtr window, uint flags);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    internal static extern bool SetWindowPos(
        IntPtr window,
        IntPtr insertAfter,
        int x,
        int y,
        int width,
        int height,
        uint flags
    );
}
