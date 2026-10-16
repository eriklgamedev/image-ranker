// Loaded only by an explicitly scheduled Windows qualification run.
// No app launch/termination, privilege adjustment, global logger or stop-by-name.
using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Globalization;
using System.IO;
using System.Runtime.InteropServices;
using System.Security.Cryptography;
using System.Text;
using Microsoft.Win32.SafeHandles;

namespace ImageRanker.Qualification {
  public static class TraceProcess {
    [StructLayout(LayoutKind.Sequential)] struct FileTime { public uint Low, High; }
    [DllImport("kernel32.dll", SetLastError=true)] static extern SafeProcessHandle OpenProcess(uint access, bool inherit, uint pid);
    [DllImport("kernel32.dll", SetLastError=true)] static extern bool GetProcessTimes(SafeProcessHandle handle, out FileTime created, out FileTime exit, out FileTime kernel, out FileTime user);
    [DllImport("kernel32.dll", SetLastError=true, CharSet=CharSet.Unicode)] static extern bool QueryFullProcessImageNameW(SafeProcessHandle handle, uint flags, StringBuilder path, ref uint length);
    [DllImport("kernel32.dll", SetLastError=true)] static extern uint WaitForSingleObject(SafeProcessHandle handle, uint milliseconds);
    public static SafeProcessHandle Open(uint pid) {
      var handle=OpenProcess(0x1000|0x100000,false,pid); // query limited + synchronize
      if(handle.IsInvalid){int error=Marshal.GetLastWin32Error();handle.Dispose();throw new Win32Exception(error,"OpenProcess");}
      return handle;
    }
    public static string Created(SafeProcessHandle handle) {
      FileTime c,x,k,u;if(!GetProcessTimes(handle,out c,out x,out k,out u))throw new Win32Exception(Marshal.GetLastWin32Error(),"GetProcessTimes");
      return (((ulong)c.High<<32)|c.Low).ToString(CultureInfo.InvariantCulture);
    }
    public static bool Alive(SafeProcessHandle handle) {
      uint result=WaitForSingleObject(handle,0);if(result==258)return true;if(result==0)return false;
      throw new Win32Exception(Marshal.GetLastWin32Error(),"WaitForSingleObject");
    }
    public static string Image(SafeProcessHandle handle) {
      uint length=32768;var value=new StringBuilder((int)length);
      if(!QueryFullProcessImageNameW(handle,0,value,ref length))throw new Win32Exception(Marshal.GetLastWin32Error(),"QueryFullProcessImageName");
      return value.ToString();
    }
    public static string CanonicalPath(string value) {
      string path=Path.GetFullPath(value);
      if(path.StartsWith(@"\\?\UNC\",StringComparison.OrdinalIgnoreCase))return @"\\"+path.Substring(8);
      if(path.StartsWith(@"\\?\",StringComparison.Ordinal))return path.Substring(4);
      return path;
    }
    public static FileStream VerifyImage(SafeProcessHandle process, string expectedPath, string expectedHash) {
      string actual=CanonicalPath(Image(process));
      if(!String.Equals(actual,CanonicalPath(expectedPath),StringComparison.OrdinalIgnoreCase))throw new InvalidOperationException("Root executable path mismatch");
      var pin=PinImage(actual,expectedHash);
      try {if(!Alive(process))throw new InvalidOperationException("Root exited during native binding");return pin;}
      catch {pin.Dispose();throw;}
    }
    public static FileStream PinImage(string path,string expectedHash) {
      var pin=new FileStream(Path.GetFullPath(path),FileMode.Open,FileAccess.Read,FileShare.Read);
      try {
        if(pin.Length>1024L*1024*1024)throw new InvalidOperationException("Executable hash input exceeds bound");
        string hash;using(var algorithm=SHA256.Create())hash=BitConverter.ToString(algorithm.ComputeHash(pin)).Replace("-","").ToLowerInvariant();
        if(!String.Equals(hash,expectedHash,StringComparison.OrdinalIgnoreCase))throw new InvalidOperationException("Root executable SHA-256 mismatch");
        pin.Position=0;return pin;
      } catch {pin.Dispose();throw;}
    }
  }

  public sealed class TraceStatistics {
    public uint EventsLost, LogBuffersLost, RealTimeBuffersLost, BuffersWritten, NumberOfBuffers, FreeBuffers;
    public string Handle, Guid, Name, Path;
  }
  public sealed class DecodeResult {
    public uint ConsumerLost;
    public long Events, Bytes, DecodeErrors;
    public bool Complete;
  }
  public sealed class OwnedTrace : IDisposable {
    const uint Query=0, Stop=1;
    static readonly Guid KernelProcess=new Guid("22fb2cd6-0e7b-422b-a0c7-2fad1fd0e716");
    static readonly Guid ClassicProcess=new Guid("3d6fa8d0-fe05-11d0-9dda-00c04fd7ba7c");
    const string SessionName="ImageRankerQualificationProcess";
    [StructLayout(LayoutKind.Sequential)] struct Wnode {
      public uint BufferSize,ProviderId; public ulong HistoricalContext; public long TimeStamp;
      public Guid Guid; public uint ClientContext,Flags;
    }
    [StructLayout(LayoutKind.Sequential)] struct Properties {
      public Wnode Wnode;public uint BufferSize,MinimumBuffers,MaximumBuffers,MaximumFileSize,LogFileMode,FlushTimer,EnableFlags;
      public int AgeLimit; public uint NumberOfBuffers,FreeBuffers,EventsLost,BuffersWritten,LogBuffersLost,RealTimeBuffersLost;
      public IntPtr LoggerThreadId;public uint LogFileNameOffset,LoggerNameOffset;
    }
    [DllImport("advapi32.dll",CharSet=CharSet.Unicode)] static extern uint StartTraceW(out ulong handle,string name,IntPtr properties);
    [DllImport("advapi32.dll",CharSet=CharSet.Unicode)] static extern uint ControlTraceW(ulong handle,string name,IntPtr properties,uint control);
    [DllImport("advapi32.dll")] static extern uint EnableTraceEx2(ulong handle,ref Guid provider,uint control,byte level,ulong anyKeyword,ulong allKeyword,uint timeout,IntPtr parameters);
    readonly Guid guid=Guid.NewGuid(); readonly string path; readonly uint maxMiB;
    ulong handle; bool started,stopped;
    public string Handle {get{return handle.ToString(CultureInfo.InvariantCulture);}}
    public string GuidText {get{return guid.ToString("D");}}
    public string Name {get{return SessionName;}}
    public string LogPath {get{return path;}}
    public bool HasStarted {get{return started;}}
    public TraceStatistics LastStatistics {get;private set;}
    public OwnedTrace(string outputPath,uint maximumMiB) {
      if(IntPtr.Size!=8 || Environment.OSVersion.Platform!=PlatformID.Win32NT)throw new PlatformNotSupportedException("Windows x64 required");
      if(maximumMiB<16||maximumMiB>1024)throw new ArgumentOutOfRangeException("maximumMiB");
      path=Path.GetFullPath(outputPath);maxMiB=maximumMiB;
      if(path.Length>32700)throw new ArgumentOutOfRangeException("outputPath");
      if(File.Exists(path))throw new IOException("Trace output already exists");
    }
    IntPtr Allocate() {
      int size=Marshal.SizeOf(typeof(Properties));
      byte[] name=Encoding.Unicode.GetBytes(SessionName+"\0"),file=Encoding.Unicode.GetBytes(path+"\0");
      int total=checked(size+name.Length+file.Length+4096);
      IntPtr memory=Marshal.AllocHGlobal(total);Marshal.Copy(new byte[total],0,memory,total);
      var value=new Properties();value.Wnode.BufferSize=(uint)total;value.Wnode.Guid=guid;value.Wnode.ClientContext=1;value.Wnode.Flags=0x20000;
      value.BufferSize=64;value.MinimumBuffers=64;value.MaximumBuffers=64;value.MaximumFileSize=maxMiB;
      value.LogFileMode=0x02000000|1; // distinct SYSTEM_LOGGER, sequential (not circular)
      value.FlushTimer=1;value.EnableFlags=1|8; // PROCESS + PROCESS_COUNTERS
      value.LoggerNameOffset=(uint)size;value.LogFileNameOffset=(uint)(size+name.Length);
      Marshal.StructureToPtr(value,memory,false);Marshal.Copy(name,0,IntPtr.Add(memory,size),name.Length);Marshal.Copy(file,0,IntPtr.Add(memory,size+name.Length),file.Length);
      return memory;
    }
    static void Check(uint result,string action) {
      if(result!=0)throw new Win32Exception((int)result,action+" failed (Win32 "+result+"); no privilege or foreign-session changes attempted");
    }
    public TraceStatistics Start() {
      if(started)throw new InvalidOperationException("Trace already started");
      IntPtr memory=Allocate();
      try {Check(StartTraceW(out handle,SessionName,memory),"StartTrace");started=true;}
      finally {Marshal.FreeHGlobal(memory);}
      try {
        Guid provider=KernelProcess;
        Check(EnableTraceEx2(handle,ref provider,1,5,0x10,0,5000,IntPtr.Zero),"EnableTraceEx2 Kernel-Process identities");
        return QueryStatistics();
      } catch(Exception original) {
        try{StopOwned();}catch(Exception cleanup){throw new AggregateException("Trace prerequisite and owned cleanup both failed",original,cleanup);}
        throw;
      }
    }
    TraceStatistics Read(IntPtr memory) {
      var value=(Properties)Marshal.PtrToStructure(memory,typeof(Properties));
      if(value.Wnode.BufferSize>131072 || value.LoggerNameOffset<Marshal.SizeOf(typeof(Properties)) || value.LogFileNameOffset<Marshal.SizeOf(typeof(Properties)) || value.LoggerNameOffset>=value.Wnode.BufferSize || value.LogFileNameOffset>=value.Wnode.BufferSize)throw new InvalidDataException("Invalid returned trace string offsets");
      string name=ReturnedString(memory,value.LoggerNameOffset,value.Wnode.BufferSize);
      string file=ReturnedString(memory,value.LogFileNameOffset,value.Wnode.BufferSize);
      if(value.Wnode.Guid!=guid || !String.Equals(name,SessionName,StringComparison.Ordinal) || !String.Equals(TraceProcess.CanonicalPath(file),TraceProcess.CanonicalPath(path),StringComparison.OrdinalIgnoreCase))throw new InvalidOperationException("Owned trace identity mismatch; refusing control");
      return new TraceStatistics {EventsLost=value.EventsLost,LogBuffersLost=value.LogBuffersLost,RealTimeBuffersLost=value.RealTimeBuffersLost,BuffersWritten=value.BuffersWritten,NumberOfBuffers=value.NumberOfBuffers,FreeBuffers=value.FreeBuffers,Handle=Handle,Guid=GuidText,Name=name,Path=file};
    }
    static string ReturnedString(IntPtr memory,uint offset,uint size) {
      int count=(int)((size-offset)/2);string value=Marshal.PtrToStringUni(IntPtr.Add(memory,(int)offset),count);
      int end=value.IndexOf('\0');if(end<0)throw new InvalidDataException("Unterminated returned trace string");return value.Substring(0,end);
    }
    public TraceStatistics QueryStatistics() {
      if(!started||stopped)throw new InvalidOperationException("No live owned trace");
      IntPtr memory=Allocate();try{Check(ControlTraceW(handle,null,memory,Query),"Query owned trace");LastStatistics=Read(memory);return LastStatistics;}finally{Marshal.FreeHGlobal(memory);}
    }
    public TraceStatistics StopOwned() {
      if(stopped)return LastStatistics;
      if(!started)throw new InvalidOperationException("No trace was started");
      QueryStatistics(); // Query exact retained handle and verify all identity fields before stopping.
      IntPtr memory=Allocate();try{Check(ControlTraceW(handle,null,memory,Stop),"Stop exact owned trace");LastStatistics=Read(memory);stopped=true;return LastStatistics;}finally{Marshal.FreeHGlobal(memory);}
    }
    public void Dispose(){if(started&&!stopped)StopOwned();}

    [StructLayout(LayoutKind.Sequential)] struct EventDescriptor {public ushort Id;public byte Version,Channel,Level,Opcode;public ushort Task;public ulong Keyword;}
    [StructLayout(LayoutKind.Sequential)] struct EventHeader {
      public ushort Size,HeaderType,Flags,EventProperty;public uint ThreadId,ProcessId;public long TimeStamp;
      public Guid Provider;public EventDescriptor Descriptor;public ulong ProcessorTime;public Guid Activity;
    }
    [StructLayout(LayoutKind.Sequential)] struct EventRecord {
      public EventHeader Header;public uint BufferContext;public ushort ExtendedDataCount,UserDataLength;
      public IntPtr ExtendedData,UserData,UserContext;
    }
    // Windows x64 EVENT_TRACE_LOGFILEW. Explicit offsets include the legacy
    // EVENT_TRACE and TRACE_LOGFILE_HEADER unions. Native layout is a gate.
    [StructLayout(LayoutKind.Explicit,Size=448)] struct Logfile {
      [FieldOffset(0)] public IntPtr LogFileName;
      [FieldOffset(8)] public IntPtr LoggerName;
      [FieldOffset(28)] public uint ProcessTraceMode;
      [FieldOffset(400)] public IntPtr BufferCallback;
      [FieldOffset(416)] public uint EventsLost;
      [FieldOffset(424)] public IntPtr EventRecordCallback;
      [FieldOffset(440)] public IntPtr Context;
    }
    [StructLayout(LayoutKind.Sequential)] struct PropertyDescriptor {public ulong PropertyName;public uint ArrayIndex,Reserved;}
    [UnmanagedFunctionPointer(CallingConvention.Winapi)] delegate void EventCallback(IntPtr record);
    [UnmanagedFunctionPointer(CallingConvention.Winapi)] delegate uint BufferCallback(IntPtr logfile);
    [DllImport("advapi32.dll",CharSet=CharSet.Unicode,SetLastError=true)] static extern ulong OpenTraceW(ref Logfile logfile);
    [DllImport("advapi32.dll")] static extern uint ProcessTrace([In] ulong[] handles,uint count,IntPtr start,IntPtr end);
    [DllImport("advapi32.dll")] static extern uint CloseTrace(ulong handle);
    [DllImport("tdh.dll")] static extern uint TdhGetPropertySize(IntPtr record,uint contextCount,IntPtr context,uint count,ref PropertyDescriptor descriptor,out uint size);
    [DllImport("tdh.dll")] static extern uint TdhGetProperty(IntPtr record,uint contextCount,IntPtr context,uint count,ref PropertyDescriptor descriptor,uint size,byte[] value);
    static ulong Unsigned(IntPtr record,string name,int expectedBytes) {
      IntPtr text=Marshal.StringToHGlobalUni(name);
      try {
        var descriptor=new PropertyDescriptor {PropertyName=(ulong)text.ToInt64(),ArrayIndex=UInt32.MaxValue};uint size;
        Check(TdhGetPropertySize(record,0,IntPtr.Zero,1,ref descriptor,out size),"TDH property "+name);
        if(size!=expectedBytes)throw new InvalidDataException("Unsupported TDH field width: "+name);
        byte[] value=new byte[size];Check(TdhGetProperty(record,0,IntPtr.Zero,1,ref descriptor,size,value),"TDH value "+name);
        return size==4?BitConverter.ToUInt32(value,0):BitConverter.ToUInt64(value,0);
      } finally {Marshal.FreeHGlobal(text);}
    }
    static string N(ulong value){return value.ToString(CultureInfo.InvariantCulture);}
    static string Q(ulong value){return "\""+N(value)+"\"";}
    static string Metadata(EventRecord value){return ",\"provider\":\""+value.Header.Provider.ToString("D")+"\",\"version\":"+value.Header.Descriptor.Version+",\"opcode\":"+value.Header.Descriptor.Opcode+",\"eventId\":"+value.Header.Descriptor.Id;}
    public static DecodeResult Decode(string etlPath,string jsonlPath,long maximumBytes,int maximumEvents) {
      if(IntPtr.Size!=8)throw new PlatformNotSupportedException("Windows x64 decoder only");
      if(maximumBytes<65536||maximumBytes>64L*1024*1024||maximumEvents<1||maximumEvents>250000)throw new ArgumentOutOfRangeException("Decoder bounds");
      var result=new DecodeResult();var encoding=new UTF8Encoding(false);bool abort=false;
      var decodeClock=System.Diagnostics.Stopwatch.StartNew();
      using(var output=new FileStream(jsonlPath,FileMode.CreateNew,FileAccess.Write,FileShare.Read)) {
        Action<string> write=line=>{
          byte[] bytes=encoding.GetBytes(line+"\n");
          if(result.Bytes+bytes.Length>maximumBytes||result.Events>=maximumEvents){abort=true;throw new InvalidDataException("Decoded output bound reached");}
          output.Write(bytes,0,bytes.Length);result.Bytes+=bytes.Length;result.Events++;
        };
        EventCallback onEvent=pointer=>{
          if(abort)return;
          var record=(EventRecord)Marshal.PtrToStructure(pointer,typeof(EventRecord));
          ulong at=(ulong)record.Header.TimeStamp;
          try {
            if(record.Header.Provider==KernelProcess) {
              ushort id=record.Header.Descriptor.Id;byte version=record.Header.Descriptor.Version;
              if(id!=1&&id!=2)return; // other manifest events are not process lifetimes
              if(version>2)throw new InvalidDataException("Unsupported Kernel-Process version");
              ulong pid=Unsigned(pointer,"ProcessID",4),created=Unsigned(pointer,"CreateTime",8);
              if(pid==0)return;
              if(id==1) {
                ulong parent=Unsigned(pointer,"ParentProcessID",4);
                if(parent==0)return; // system roots cannot be children of this post-ready app
                write("{\"type\":\"start\",\"at\":"+Q(at)+",\"pid\":"+N(pid)+",\"created\":"+Q(created)+",\"parentPid\":"+N(parent)+Metadata(record)+"}");
              } else {
                ulong exit=Unsigned(pointer,"ExitTime",8);
                write("{\"type\":\"stop\",\"at\":"+Q(at)+",\"pid\":"+N(pid)+",\"created\":"+Q(created)+",\"exit\":"+Q(exit)+Metadata(record)+"}");
              }
            } else if(record.Header.Provider==ClassicProcess && record.Header.Descriptor.Opcode==32) {
              if(record.Header.Descriptor.Version!=2)throw new InvalidDataException("Unsupported process counter version");
              ulong pid=Unsigned(pointer,"ProcessId",4);
              int size=(record.Header.Flags&0x20)!=0?4:8; // TDH SizeT follows event pointer width
              ulong peak=Unsigned(pointer,"PeakWorkingSetSize",size);
              if(pid!=0)write("{\"type\":\"terminal-counter\",\"at\":"+Q(at)+",\"pid\":"+N(pid)+",\"peakWorkingSetBytes\":"+Q(peak)+Metadata(record)+"}");
            }
          } catch {
            result.DecodeErrors++;
            try {write("{\"type\":\"issue\",\"at\":"+Q(at)+",\"reason\":\"unsupported-or-invalid-native-event-schema\"}");}catch{abort=true;}
          }
        };
        BufferCallback onBuffer=pointer=>{
          var file=(Logfile)Marshal.PtrToStructure(pointer,typeof(Logfile));result.ConsumerLost=Math.Max(result.ConsumerLost,file.EventsLost);
          if(decodeClock.Elapsed.TotalSeconds>30){abort=true;result.DecodeErrors++;}
          return abort?0u:1u;
        };
        IntPtr name=Marshal.StringToHGlobalUni(Path.GetFullPath(etlPath));ulong trace=UInt64.MaxValue;
        try {
          var file=new Logfile {LogFileName=name,ProcessTraceMode=0x10000000,EventRecordCallback=Marshal.GetFunctionPointerForDelegate(onEvent),BufferCallback=Marshal.GetFunctionPointerForDelegate(onBuffer)};
          trace=OpenTraceW(ref file);if(trace==UInt64.MaxValue)throw new Win32Exception(Marshal.GetLastWin32Error(),"OpenTrace");
          uint code=ProcessTrace(new[]{trace},1,IntPtr.Zero,IntPtr.Zero);
          result.ConsumerLost=Math.Max(result.ConsumerLost,file.EventsLost);
          if(code!=0&&!abort)Check(code,"ProcessTrace");
          result.Complete=code==0&&!abort;output.Flush(true);
        } finally {
          if(trace!=UInt64.MaxValue)CloseTrace(trace);
          Marshal.FreeHGlobal(name);GC.KeepAlive(onEvent);GC.KeepAlive(onBuffer);
        }
      }
      return result;
    }
  }
}
