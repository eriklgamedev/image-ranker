// Reusable native test harness; never bundled into the shipping application.
// @wdio/tauri-service 1.4.0's launcher rejects the current Microsoft Edge WebDriver banner.
// Keep its worker-side API, but own the official driver's process lifecycle without a shell.
import TauriWorkerService from '@wdio/tauri-service';
import {SevereServiceError} from 'webdriverio';
import {spawn, execFile} from 'node:child_process';
import {promisify} from 'node:util';
import fs from 'node:fs';
import path from 'node:path';
import net from 'node:net';
import {setTimeout as delay} from 'node:timers/promises';

const execute = promisify(execFile);
async function assertPortFree(port) {
  const server = net.createServer();
  await new Promise((resolve,reject) => {
    server.once('error',reject);
    server.listen(port,'127.0.0.1',resolve);
  });
  await new Promise((resolve,reject)=>server.close(error=>error?reject(error):resolve()));
}

export default class WindowsDriverService extends TauriWorkerService {
  async onPrepare(config, capabilities) {
    try {
      const root=process.env.IMAGE_RANKER_E2E_ROOT;
      if (!root || fs.readFileSync(path.join(root,'.image-ranker-e2e'),'utf8').trim()!=='image-ranker-disposable-fixture-v1') throw new Error('Missing disposable fixture root');
      const driver=path.resolve('.cargo-home/bin/tauri-driver.exe');
      const edge=path.resolve('.drivers/msedgedriver.exe');
      const {stdout}=await execute(edge,['--version'],{windowsHide:true,timeout:5000});
      const version=stdout.match(/(?:MSEdgeDriver|Microsoft Edge WebDriver)\s+(\d+\.\d+\.\d+\.\d+)/)?.[1];
      if (!version) throw new Error(`Unrecognized EdgeDriver version: ${stdout.trim()}`);
      const {stdout:registry}=await execute('reg.exe',['query','HKLM\\SOFTWARE\\WOW6432Node\\Microsoft\\EdgeUpdate\\Clients\\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}','/v','pv'],{windowsHide:true,timeout:5000});
      const runtime=registry.match(/pv\s+REG_SZ\s+(\d+\.\d+\.\d+\.\d+)/)?.[1];
      if (!runtime || version.split('.').slice(0,3).join('.')!==runtime.split('.').slice(0,3).join('.')) throw new Error(`EdgeDriver ${version} does not match installed WebView2 ${runtime}`);
      const port=config.port||4444, nativePort=port+1;
      await assertPortFree(port); await assertPortFree(nativePort);
      for(const cap of capabilities) delete cap.browserName;
      fs.mkdirSync(path.join(root,'driver-logs'),{recursive:true});
      this.log=fs.createWriteStream(path.join(root,'driver-logs','native-driver.log'),{flags:'a'});
      this.child=spawn(driver,['--port',String(port),'--native-port',String(nativePort),'--native-driver',edge],{windowsHide:true,shell:false,env:process.env,stdio:['ignore','pipe','pipe']});
      process.env.IMAGE_RANKER_DRIVER_PID=String(this.child.pid);
      this.child.stdout.pipe(this.log,{end:false}); this.child.stderr.pipe(this.log,{end:false});
      let failure;
      this.child.once('error',error=>failure=error);
      this.child.once('exit',(code,signal)=>failure=new Error(`Driver exited: ${code??signal}`));
      const deadline=Date.now()+30000;
      while(Date.now()<deadline) {
        if(failure) throw failure;
        try {const response=await fetch(`http://127.0.0.1:${port}/status`,{signal:AbortSignal.timeout(1000)});if(response.ok) {console.log(`Native test driver ready; EdgeDriver ${version}, WebView2 ${runtime}`);return;}} catch { /* Wait for this known child to bind its port. */ }
        await delay(100);
      }
      throw new Error('Official driver failed to become ready; inspect native-driver.log');
    } catch(error) {
      await this.onComplete();
      throw new SevereServiceError(error.message);
    }
  }
  async onComplete() {
    if(this.child && this.child.exitCode===null && this.child.signalCode===null) {
      const exited=new Promise(resolve=>this.child.once('exit',resolve));
      this.child.kill();
      await Promise.race([exited,delay(5000)]);
    }
    this.log?.end();
  }
}
