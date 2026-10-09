import { createApp } from 'vue'
import VxeUIAll from 'vxe-pc-ui'
import 'vxe-pc-ui/lib/style.css'
import VxeUITable from 'vxe-table'
import 'vxe-table/lib/style.css'
import App from './App.vue'
import './styles.css'

const app = createApp(App)
app.use(VxeUIAll).use(VxeUITable)
app.mount('#app')
