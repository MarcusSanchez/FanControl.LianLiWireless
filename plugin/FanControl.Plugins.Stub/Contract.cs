using System;
using System.Collections.Generic;
using System.Threading.Tasks;

namespace FanControl.Plugins
{
    public interface IPlugin
    {
        string Name { get; }

        void Initialize();

        void Load(IPluginSensorsContainer _container);

        void Close();
    }

    public interface IPlugin2 : IPlugin
    {
        void Update();
    }

    public interface IPlugin3 : IPlugin2
    {
        event Action RefreshRequested;
    }

    public interface IPluginApplicationControl
    {
        void ShowMainWindow();
    }

    public interface IPluginDialog
    {
        Task ShowMessageDialog(string message);
    }

    public interface IPluginLogger
    {
        void Log(string message);
    }

    public interface IPluginSensor
    {
        string Id { get; }

        string Name { get; }

        float? Value { get; }

        void Update();
    }

    public interface IPluginControlSensor : IPluginSensor
    {
        void Set(float val);

        void Reset();
    }

    public interface IPluginControlSensor2 : IPluginControlSensor
    {
        string PairedFanSensorId { get; }
    }

    public interface IPluginSensorsContainer
    {
        List<IPluginControlSensor> ControlSensors { get; }

        List<IPluginSensor> FanSensors { get; }

        List<IPluginSensor> TempSensors { get; }
    }
}
